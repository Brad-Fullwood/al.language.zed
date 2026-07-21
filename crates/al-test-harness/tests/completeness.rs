//! Response-shape and state-transition tests for declared LSP capabilities.

use al_test_harness::*;

const CALLER_CU: &str = r#"codeunit 50100 "Caller CU"
{
    procedure DoWork()
    var
        Helper: Codeunit "Helper CU";
        Result: Text;
    begin
        Result := Helper.Calculate(10, 20);
        Message(Result);
    end;
}"#;

const HELPER_CU: &str = r#"codeunit 50101 "Helper CU"
{
    procedure Calculate(Width: Integer; Height: Integer): Text
    begin
        exit(Format(Width * Height));
    end;

    procedure Validate(Input: Text): Boolean
    begin
        exit(Input <> '');
    end;
}"#;

const TABLE_WITH_FIELDS: &str = r#"table 50100 "Test Item"
{
    fields
    {
        field(1; "No."; Code[20])
        {
            trigger OnValidate()
            begin
                if "No." = '' then
                    Error('No. must not be empty');
            end;
        }
        field(2; "Description"; Text[100]) { }
        field(3; "Unit Price"; Decimal) { }
        field(4; "Quantity"; Integer) { }
        field(5; "Active"; Boolean) { }
    }

    keys
    {
        key(PK; "No.") { Clustered = true; }
        key(Desc; "Description") { }
    }
}"#;

const ENUM_AL: &str = r#"enum 50100 "Item Status"
{
    Extensible = true;

    value(0; "Active") { Caption = 'Active'; }
    value(1; "Inactive") { Caption = 'Inactive'; }
    value(2; "Blocked") { Caption = 'Blocked'; }
}"#;

const MULTI_PARAM_CU: &str = r#"codeunit 50103 "Multi Param"
{
    procedure ThreeParams(A: Integer; B: Text; C: Boolean)
    begin
    end;

    procedure Caller()
    begin
        ThreeParams(1, 'hello', true);
    end;
}"#;

#[tokio::test]
async fn test_completeness_a01_diagnostic_ranges_are_valid() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let code_with_lint = r#"codeunit 50100 "Diag Range"
{
    procedure EmptyProc()
    begin
    end;

    procedure WithTodo()
    begin
        // TODO: fix this
    end;
}"#;
    client.open_file("src/diag_range.al", code_with_lint).await;

    let diags = client.drain_diagnostics();
    let uri = client.file_uri("src/diag_range.al");
    if let Some(file_diags) = diags.get(&uri) {
        for diag in file_diags {
            let range = diag.get("range").expect("diagnostic must have range");
            let start = range.get("start").expect("range must have start");
            let end = range.get("end").expect("range must have end");

            let start_line = start
                .get("line")
                .and_then(|v| v.as_u64())
                .expect("start must have line");
            let start_char = start
                .get("character")
                .and_then(|v| v.as_u64())
                .expect("start must have character");
            let end_line = end
                .get("line")
                .and_then(|v| v.as_u64())
                .expect("end must have line");
            let end_char = end
                .get("character")
                .and_then(|v| v.as_u64())
                .expect("end must have character");

            assert!(
                end_line >= start_line,
                "diagnostic end line must be >= start line: {diag}"
            );
            if end_line == start_line {
                assert!(
                    end_char >= start_char,
                    "diagnostic end char must be >= start char on same line: {diag}"
                );
            }

            assert!(
                diag.get("severity").is_some(),
                "diagnostic must have severity: {diag}"
            );

            let msg = diag.get("message").and_then(|v| v.as_str());
            assert!(
                msg.is_some() && !msg.unwrap().is_empty(),
                "diagnostic must have non-empty message: {diag}"
            );
        }
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_completeness_a02_diagnostic_codes_are_strings() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let code = r#"codeunit 50100 "Diag Code"
{
    procedure EmptyProc()
    begin
    end;
}"#;
    client.open_file("src/diag_code.al", code).await;

    let diags = client.drain_diagnostics();
    let uri = client.file_uri("src/diag_code.al");

    // The server must publish a diagnostics notification for the opened file.
    // Native lint rules have been removed, so the array will be empty, but the
    // notification itself must still arrive.
    assert!(
        diags.contains_key(&uri),
        "server must publish diagnostics notification for opened file (even if empty): keys={:?}",
        diags.keys().collect::<Vec<_>>()
    );

    // If any diagnostics are present (e.g. from future rules or .NET bridge),
    // every code field must be a string starting with "AL-".
    let file_diags = diags.get(&uri).map(Vec::as_slice).unwrap_or_default();
    for diag in file_diags {
        if let Some(code) = diag.get("code") {
            // Zed displays diagnostic codes — they should be strings
            assert!(
                code.is_string(),
                "diagnostic code should be a string for Zed display: {diag}"
            );
            let code_str = code.as_str().unwrap();
            assert!(
                code_str.starts_with("AL-"),
                "AL lint codes should start with 'AL-': got {code_str}"
            );
        }
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_completeness_b01_completion_items_have_kind() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let code = r#"codeunit 50100 "Comp Kind"
{
    procedure Test()
    var
        Msg: Text;
    begin
        Msg := 'hello';

    end;
}"#;
    client.open_file("src/comp_kind.al", code).await;

    let items = client.completion("src/comp_kind.al", 7, 8).await;
    assert!(
        !items.is_empty(),
        "should have completions in procedure body"
    );

    for item in &items {
        // Zed uses kind for icons — must be present
        assert!(
            item.get("label").and_then(|v| v.as_str()).is_some(),
            "completion item must have label: {item}"
        );
        assert!(
            item.get("kind").is_some(),
            "completion item must have kind for Zed icon: {item}"
        );
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_completeness_b02_completion_items_have_detail() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let code = r#"codeunit 50100 "Comp Detail"
{
    procedure MyHelper(A: Integer): Text
    begin
        exit(Format(A));
    end;

    procedure Caller()
    begin

    end;
}"#;
    client.open_file("src/comp_detail.al", code).await;

    let items = client.completion("src/comp_detail.al", 9, 8).await;

    let helper = items
        .iter()
        .find(|i| i.get("label").and_then(|l| l.as_str()) == Some("MyHelper"));
    if let Some(h) = helper {
        let detail = h.get("detail").and_then(|d| d.as_str());
        assert!(
            detail.is_some(),
            "procedure completion should have detail: {h}"
        );
        if let Some(d) = detail {
            assert!(
                d.contains("Integer") || d.contains("Text"),
                "detail should show parameter/return types: {d}"
            );
        }
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_completeness_c01_semantic_tokens_cover_all_token_types() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let code = r#"codeunit 50100 "Token Types"
{
    var
        GlobalVar: Integer;

    procedure Process(Input: Text): Boolean
    var
        LocalVar: Decimal;
    begin
        // A comment line
        LocalVar := 3.14;
        GlobalVar := 42;
        if Input <> '' then
            exit(true);
        exit(false);
    end;
}"#;
    client.open_file("src/token_types.al", code).await;

    let tokens = client.semantic_tokens("src/token_types.al").await;
    assert!(tokens.is_some(), "should produce semantic tokens");

    let data = semantic_token_data(&tokens.unwrap());
    assert!(
        data.len() >= 10,
        "rich code should produce at least 10 token groups: got {}",
        data.len()
    );

    for (i, group) in data.iter().enumerate() {
        assert!(group[2] > 0, "token {i} length must be > 0: {group:?}");
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_completeness_d01_cross_file_hover_after_edit() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    client.open_file("src/helper.al", HELPER_CU).await;
    client.open_file("src/caller.al", CALLER_CU).await;

    let edited_helper = HELPER_CU.replace(
        "    procedure Validate",
        "    procedure NewMethod(X: Text): Integer\n    begin\n        exit(StrLen(X));\n    end;\n\n    procedure Validate"
    );
    client.change_file("src/helper.al", &edited_helper).await;

    let hover = client.hover("src/caller.al", 4, 28).await;
    let content = hover.as_ref().and_then(hover_content);
    assert!(
        content.is_some_and(|text| text.contains("Helper CU")),
        "cross-file hover disappeared after edit: {content:?}"
    );
    let symbols = client.workspace_symbol("Helper CU").await;
    assert!(
        !symbols.is_empty(),
        "workspace should still find Helper CU after edit"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_completeness_d02_workspace_symbols_reflect_edits() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    client
        .open_file(
            "src/wsym_test.al",
            r#"codeunit 50100 "Original Name"
{
    procedure Foo()
    begin
    end;
}"#,
        )
        .await;

    let syms = client.workspace_symbol("Original Name").await;
    assert!(!syms.is_empty(), "should find 'Original Name'");

    // Edit: change the object name
    client
        .change_file(
            "src/wsym_test.al",
            r#"codeunit 50100 "Changed Name"
{
    procedure Bar()
    begin
    end;
}"#,
        )
        .await;

    let syms_new = client.workspace_symbol("Changed Name").await;
    let syms_old = client.workspace_symbol("Original Name").await;
    assert!(!syms_new.is_empty(), "changed object name was not indexed");
    assert!(
        syms_old.iter().all(|s| {
            s.get("name").and_then(|n| n.as_str()) != Some("Original Name")
                || s.get("location").is_none()
        }),
        "stale object name remained indexed: {syms_old:?}"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_completeness_d03_close_file_clears_diagnostics() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let code = r#"codeunit 50100 "Close Diag"
{
    procedure EmptyProc()
    begin
    end;
}"#;
    client.open_file("src/close_diag.al", code).await;

    // The server should publish a diagnostics notification on open
    // (even if the array is empty — native lint rules have been removed).
    let diags1 = client.drain_diagnostics();
    let uri = client.file_uri("src/close_diag.al");
    assert!(
        diags1.contains_key(&uri),
        "server should publish diagnostics notification after open"
    );

    // Close the file
    client.close_file("src/close_diag.al").await;

    // Give server a moment to process and publish cleared diagnostics
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    let symbols = client.workspace_symbol("").await;
    assert!(!symbols.is_empty(), "server should work after close");

    let diags2 = client.drain_diagnostics();
    // After closing the file, the server should either publish an empty
    // diagnostics array for the closed URI, or stop publishing for it.
    if let Some(closed_diags) = diags2.get(&uri) {
        assert!(
            closed_diags.is_empty(),
            "diagnostics for closed file must be empty, got: {:?}",
            closed_diags
        );
    }
    // If the URI is absent, that is also acceptable (server stopped reporting)

    client.shutdown().await;
}

#[tokio::test]
async fn test_completeness_e01_signature_help_at_open_paren() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    client.open_file("src/sig_trigger.al", MULTI_PARAM_CU).await;

    // Signature help at the '(' position: ThreeParams(
    // Line 8: "        ThreeParams(1, 'hello', true);"
    // The '(' is at approximately col 19
    let sig = client.signature_help("src/sig_trigger.al", 8, 20).await;
    if let Some(result) = &sig {
        let sigs = result.get("signatures").and_then(|s| s.as_array());
        assert!(
            sigs.is_some() && !sigs.unwrap().is_empty(),
            "signature help at ( should return signatures: {result}"
        );

        // Each signature should have a label
        for s in sigs.unwrap() {
            assert!(
                s.get("label").and_then(|l| l.as_str()).is_some(),
                "signature must have label: {s}"
            );
        }
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_completeness_e02_signature_help_at_comma() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    client.open_file("src/sig_comma.al", MULTI_PARAM_CU).await;

    // Signature help at comma position: ThreeParams(1, 'hello', true)
    // After the first comma, activeParameter should be 1
    let sig = client.signature_help("src/sig_comma.al", 8, 22).await;
    if let Some(result) = &sig {
        let active = result.get("activeParameter").and_then(|a| a.as_u64());
        if let Some(idx) = active {
            assert!(
                idx >= 1,
                "at comma position, activeParameter should be >= 1: {result}"
            );
        }
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_completeness_f01_folding_covers_all_object_types() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    client
        .open_file("src/fold_table.al", TABLE_WITH_FIELDS)
        .await;
    let folds = client.folding_ranges("src/fold_table.al").await;
    assert!(!folds.is_empty(), "table should have folding ranges");
    let fold_lines: Vec<(u32, u32)> = folding_range_lines(&folds);
    assert!(
        fold_lines.len() >= 2,
        "table should have folds for fields and keys: {fold_lines:?}"
    );

    client.open_file("src/fold_enum.al", ENUM_AL).await;
    let folds = client.folding_ranges("src/fold_enum.al").await;
    assert!(!folds.is_empty(), "enum should have folding ranges");

    client.open_file("src/fold_cu.al", HELPER_CU).await;
    let folds = client.folding_ranges("src/fold_cu.al").await;
    assert!(!folds.is_empty(), "codeunit should have folding ranges");

    client.shutdown().await;
}

#[tokio::test]
async fn test_completeness_f02_folding_ranges_are_line_based() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    client.open_file("src/fold_check.al", HELPER_CU).await;
    let folds = client.folding_ranges("src/fold_check.al").await;

    for fold in &folds {
        let start_line = fold.get("startLine").and_then(|v| v.as_u64());
        let end_line = fold.get("endLine").and_then(|v| v.as_u64());

        assert!(
            start_line.is_some(),
            "folding range must have startLine: {fold}"
        );
        assert!(
            end_line.is_some(),
            "folding range must have endLine: {fold}"
        );

        if let (Some(s), Some(e)) = (start_line, end_line) {
            assert!(
                e > s,
                "folding range must span at least 2 lines: start={s}, end={e}"
            );
        }
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_completeness_g01_table_symbols_have_fields_as_children() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    client
        .open_file("src/sym_table.al", TABLE_WITH_FIELDS)
        .await;
    let symbols = client.document_symbols("src/sym_table.al").await;

    assert!(!symbols.is_empty(), "table should have document symbols");

    let table_sym = symbols.iter().find(|s| {
        s.get("name")
            .and_then(|n| n.as_str())
            .map(|n| n.contains("Test Item"))
            .unwrap_or(false)
    });
    assert!(
        table_sym.is_some(),
        "should find 'Test Item' table symbol: {symbols:?}"
    );

    if let Some(table) = table_sym {
        let children = table.get("children").and_then(|c| c.as_array());
        assert!(
            children.is_some(),
            "table symbol should have children (fields)"
        );
        if let Some(kids) = children {
            // Table children may be groups (fields, keys) or individual fields depending on outline depth
            assert!(!kids.is_empty(), "table should have children: got 0");

            fn count_descendants(syms: &[serde_json::Value]) -> usize {
                let mut count = syms.len();
                for s in syms {
                    if let Some(children) = s.get("children").and_then(|c| c.as_array()) {
                        count += count_descendants(children);
                    }
                }
                count
            }
            let total = count_descendants(kids);
            assert!(
                total >= 5,
                "table outline should have at least 5 total descendant symbols: got {total}"
            );

            fn validate_symbols(syms: &[serde_json::Value]) {
                for child in syms {
                    assert!(
                        child.get("name").is_some(),
                        "symbol must have name: {child}"
                    );
                    assert!(
                        child.get("kind").is_some(),
                        "symbol must have kind: {child}"
                    );
                    assert!(
                        child.get("range").is_some(),
                        "symbol must have range: {child}"
                    );
                    if let Some(children) = child.get("children").and_then(|c| c.as_array()) {
                        validate_symbols(children);
                    }
                }
            }
            validate_symbols(kids);
        }
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_completeness_g02_codeunit_symbols_have_procedures_as_children() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    client.open_file("src/sym_cu.al", HELPER_CU).await;
    let symbols = client.document_symbols("src/sym_cu.al").await;

    let cu_sym = symbols.iter().find(|s| {
        s.get("name")
            .and_then(|n| n.as_str())
            .map(|n| n.contains("Helper"))
            .unwrap_or(false)
    });
    assert!(cu_sym.is_some(), "should find 'Helper CU' symbol");

    if let Some(cu) = cu_sym {
        let children = cu.get("children").and_then(|c| c.as_array());
        assert!(
            children.is_some(),
            "codeunit should have children (procedures)"
        );
        if let Some(kids) = children {
            let names: Vec<&str> = kids
                .iter()
                .filter_map(|c| c.get("name").and_then(|n| n.as_str()))
                .collect();
            assert!(
                names.contains(&"Calculate"),
                "should have Calculate: {names:?}"
            );
            assert!(
                names.contains(&"Validate"),
                "should have Validate: {names:?}"
            );
        }
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_completeness_g03_enum_symbols_have_values() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    client.open_file("src/sym_enum.al", ENUM_AL).await;
    let symbols = client.document_symbols("src/sym_enum.al").await;

    let enum_sym = symbols.iter().find(|s| {
        s.get("name")
            .and_then(|n| n.as_str())
            .map(|n| n.contains("Item Status"))
            .unwrap_or(false)
    });
    assert!(enum_sym.is_some(), "should find 'Item Status' enum symbol");

    if let Some(e) = enum_sym {
        let children = e.get("children").and_then(|c| c.as_array());
        assert!(children.is_some(), "enum should have children (values)");
        if let Some(kids) = children {
            let names: Vec<&str> = kids
                .iter()
                .filter_map(|c| c.get("name").and_then(|n| n.as_str()))
                .collect();
            assert!(
                names.iter().any(|n| n.contains("Active")),
                "should have Active: {names:?}"
            );
            assert!(
                names.iter().any(|n| n.contains("Blocked")),
                "should have Blocked: {names:?}"
            );
        }
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_completeness_h01_rename_produces_valid_workspace_edit() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let code = r#"codeunit 50100 "Rename Valid"
{
    procedure DoWork()
    var
        Counter: Integer;
    begin
        Counter := 0;
        Counter += 1;
        Message(Format(Counter));
    end;
}"#;
    client.open_file("src/rename_valid.al", code).await;

    let result = client
        .rename("src/rename_valid.al", 4, 10, "ItemCount")
        .await;
    assert!(result.is_some(), "rename should produce a workspace edit");

    let edit = result.unwrap();
    let has_changes = edit.get("changes").is_some() || edit.get("documentChanges").is_some();
    assert!(has_changes, "workspace edit should have changes: {edit}");

    if let Some(changes) = edit.get("changes").and_then(|c| c.as_object()) {
        for (uri, edits) in changes {
            let edits = edits.as_array().expect("changes should be array");
            assert!(!edits.is_empty(), "changes for {uri} should not be empty");
            for e in edits {
                assert!(e.get("range").is_some(), "edit must have range: {e}");
                let new_text = e.get("newText").and_then(|t| t.as_str());
                assert_eq!(
                    new_text,
                    Some("ItemCount"),
                    "newText should be 'ItemCount': {e}"
                );
            }
        }
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_completeness_h02_rename_on_keyword_returns_none() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let code = r#"codeunit 50100 "Rename Keyword"
{
    procedure Test()
    begin
    end;
}"#;
    client.open_file("src/rename_kw.al", code).await;

    // Try renaming "begin" keyword — should not be renamable
    let result = client.rename("src/rename_kw.al", 3, 4, "NewName").await;
    // Either None or empty edit is acceptable
    if let Some(edit) = &result {
        let changes = edit.get("changes").and_then(|c| c.as_object());
        if let Some(c) = changes {
            assert!(
                c.is_empty(),
                "renaming a keyword should produce no changes: {edit}"
            );
        }
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_completeness_i01_references_include_declaration() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let code = r#"codeunit 50100 "Refs Test"
{
    procedure Test()
    var
        MyVar: Integer;
    begin
        MyVar := 10;
        MyVar := MyVar + 1;
        Message(Format(MyVar));
    end;
}"#;
    client.open_file("src/refs_test.al", code).await;

    let refs = client.references("src/refs_test.al", 4, 10).await;
    // MyVar appears: declaration (line 4), assignment (line 6), assignment+use (line 7), use (line 8)
    assert!(
        refs.len() >= 3,
        "MyVar should have at least 3 references: got {}",
        refs.len()
    );

    for r in &refs {
        assert!(r.get("uri").is_some(), "reference must have uri: {r}");
        assert!(r.get("range").is_some(), "reference must have range: {r}");
    }

    client.shutdown().await;
}

#[tokio::test]
async fn core_navigation_and_symbol_requests_are_functional() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let code = r#"codeunit 50100 "Cap Test"
{
    procedure Process(Input: Text): Integer
    var
        Len: Integer;
    begin
        Len := StrLen(Input);
        // TODO: implement properly
        exit(Len);
    end;

    procedure EmptyBlock()
    begin
    end;
}"#;
    client.open_file("src/cap_test.al", code).await;

    let hover = client.hover("src/cap_test.al", 4, 10).await;
    assert!(hover.is_some(), "hover capability must work");

    let comp = client.completion("src/cap_test.al", 7, 8).await;
    assert!(!comp.is_empty(), "completion capability must work");

    let refs = client.references("src/cap_test.al", 4, 10).await;
    assert!(!refs.is_empty(), "references capability must work");

    let syms = client.document_symbols("src/cap_test.al").await;
    assert!(!syms.is_empty(), "documentSymbol capability must work");

    let toks = client.semantic_tokens("src/cap_test.al").await;
    assert!(toks.is_some(), "semanticTokens capability must work");

    let folds = client.folding_ranges("src/cap_test.al").await;
    assert!(!folds.is_empty(), "foldingRange capability must work");

    let ren = client.rename("src/cap_test.al", 4, 10, "Length").await;
    assert!(ren.is_some(), "rename capability must work");

    let ws = client.workspace_symbol("Cap Test").await;
    assert!(!ws.is_empty(), "workspace/symbol capability must work");

    client.shutdown().await;
}

#[tokio::test]
async fn test_completeness_k01_large_file_hover_is_correct() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let mut code = String::from("codeunit 50100 \"Large File\"\n{\n");
    for i in 0..100 {
        code.push_str(&format!(
            "    procedure Proc{i}(Param{i}: Integer): Text\n    begin\n        exit(Format(Param{i}));\n    end;\n\n"
        ));
    }
    code.push_str("}\n");

    client.open_file("src/large_file.al", &code).await;

    // Hover on Proc50's parameter — should correctly identify it
    // Proc50 starts at line 2 + 50*5 = 252, parameter at line 252
    // Line: "    procedure Proc50(Param50: Integer): Text"
    //        0123456789012345678901 (col 21 = 'P' of Param50)
    //        "    procedure Proc50(" = 4 + 10 + 6 + 1 = 21 chars
    let hover = client.hover("src/large_file.al", 252, 21).await;
    assert!(
        hover.is_some(),
        "hover should work in large file at procedure 50"
    );

    let hover_val = hover.unwrap();
    let content = hover_content(&hover_val);
    assert!(
        content.is_some_and(|c| c.contains("Param50") || c.contains("Integer")),
        "hover on Param50 should mention parameter name or type. Got: {:?}",
        content
    );

    let symbols = client.document_symbols("src/large_file.al").await;
    let all_names: Vec<&str> = symbol_names(&symbols);
    assert!(
        all_names.len() >= 100,
        "large file should have 100+ symbols: got {}",
        all_names.len()
    );
    assert!(all_names.contains(&"Proc0"), "should find Proc0");
    assert!(all_names.contains(&"Proc99"), "should find Proc99");

    client.shutdown().await;
}

#[tokio::test]
async fn test_completeness_l01_open_close_many_files() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    for i in 0..10 {
        let code = format!(
            r#"codeunit {id} "Multi {i}"
{{
    procedure Method{i}()
    begin
    end;
}}"#,
            id = 50100 + i
        );
        client.open_file(&format!("src/multi_{i}.al"), &code).await;
    }

    let syms = client.workspace_symbol("Multi").await;
    assert!(
        syms.len() >= 10,
        "should find all 10 opened codeunits: got {}",
        syms.len()
    );

    for i in 0..5 {
        client.close_file(&format!("src/multi_{i}.al")).await;
    }

    let syms = client.workspace_symbol("Multi").await;
    assert!(
        !syms.is_empty(),
        "should still find some codeunits after closing half"
    );

    client.shutdown().await;
}
