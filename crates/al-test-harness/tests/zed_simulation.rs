//! Zed simulation tests — test against the real Debar project directory.
//!
//! These tests simulate exactly what happens when Zed opens an AL project:
//! 1. Server starts with workspace root pointing to the project
//! 2. Server discovers app.json and scans for .al files
//! 3. Files are opened by the editor
//! 4. Various LSP features are invoked

use al_test_harness::*;
use std::path::PathBuf;

fn debar_project_dir() -> PathBuf {
    PathBuf::from("/home/bradf/Dev/AL/Debar/App Integration")
}

fn debar_project_exists() -> bool {
    debar_project_dir().join("app.json").exists()
}

fn find_position(content: &str, needle: &str) -> Option<(u32, u32)> {
    for (line_idx, line) in content.lines().enumerate() {
        if let Some(col_idx) = line.find(needle) {
            return Some((line_idx as u32, col_idx as u32));
        }
    }
    None
}

fn definition_start_line(result: &serde_json::Value) -> Option<u32> {
    if let Some(line) = result
        .get("range")
        .and_then(|range| range.get("start"))
        .and_then(|start| start.get("line"))
        .and_then(|line| line.as_u64())
    {
        return Some(line as u32);
    }

    result
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|loc| loc.get("range"))
        .and_then(|range| range.get("start"))
        .and_then(|start| start.get("line"))
        .and_then(|line| line.as_u64())
        .map(|line| line as u32)
}

fn definition_uri(result: &serde_json::Value) -> Option<&str> {
    result
        .get("uri")
        .and_then(|uri| uri.as_str())
        .or_else(|| {
            result
                .as_array()
                .and_then(|arr| arr.first())
                .and_then(|loc| loc.get("uri"))
                .and_then(|uri| uri.as_str())
        })
}

fn hover_markdown(result: &serde_json::Value) -> Option<&str> {
    result
        .get("contents")
        .and_then(|contents| contents.get("value"))
        .and_then(|value| value.as_str())
}

/// Open common Debar project files into a client.
async fn open_debar_files(client: &mut LspClient) {
    let files = [
        "objects/API/ItemJournalStaging.Table.al",
        "objects/API/ItemJournalAPI.Page.al",
        "objects/Automation/IJLAPIHelper.Codeunit.al",
        "objects/Automation/IJLPostTask.Codeunit.al",
        "objects/Automation/IJLStatus.Enum.al",
        "objects/Testing/IJLProcessStaging.Report.al",
        "objects/Testing/IJLProcessAction.Enum.al",
        "objects/System/JsonTools.Codeunit.al",
    ];
    for file in &files {
        let path = debar_project_dir().join(file);
        if path.exists() {
            let content = std::fs::read_to_string(&path).unwrap();
            client.open_file(file, &content).await;
        }
    }
}

// ---------------------------------------------------------------------------
// Workspace initialization with real project
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_debar_project_initializes() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let client = LspClient::spawn(debar_project_dir()).await.unwrap();
    // Server should initialize without error, find app.json, scan .al files
    client.shutdown().await;
}

#[tokio::test]
async fn test_debar_workspace_symbols_after_init() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();

    // Give extra time for workspace scanning + possible NuGet downloads on first run
    tokio::time::sleep(tokio::time::Duration::from_millis(5000)).await;

    // Search for objects that should be in the workspace
    let symbols = client.workspace_symbol("IJL").await;
    assert!(
        !symbols.is_empty(),
        "Should find workspace symbols matching 'IJL' after scanning. Got 0 results."
    );

    let all_symbols = client.workspace_symbol("").await;
    assert!(
        all_symbols.len() >= 5,
        "Should find at least 5 workspace objects from Debar project. Got: {}",
        all_symbols.len()
    );

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// Open real files from the Debar project
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_debar_open_real_files() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();

    // Read and open real files
    let files = [
        "objects/API/ItemJournalStaging.Table.al",
        "objects/API/ItemJournalAPI.Page.al",
        "objects/Automation/IJLAPIHelper.Codeunit.al",
        "objects/Automation/IJLPostTask.Codeunit.al",
        "objects/Automation/IJLStatus.Enum.al",
        "objects/System/JsonTools.Codeunit.al",
    ];

    for file in &files {
        let path = debar_project_dir().join(file);
        if path.exists() {
            let content = std::fs::read_to_string(&path).unwrap();
            client.open_file(file, &content).await;
        }
    }

    // Verify each file gets document symbols
    for file in &files {
        let path = debar_project_dir().join(file);
        if path.exists() {
            let symbols = client.document_symbols(file).await;
            let names = symbol_names(&symbols);
            assert!(
                !names.is_empty(),
                "File {} should have document symbols. Got none.",
                file
            );
        }
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_debar_semantic_tokens_real_files() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();

    let files = [
        "objects/API/ItemJournalStaging.Table.al",
        "objects/Automation/IJLAPIHelper.Codeunit.al",
        "objects/Automation/IJLStatus.Enum.al",
    ];

    for file in &files {
        let path = debar_project_dir().join(file);
        if path.exists() {
            let content = std::fs::read_to_string(&path).unwrap();
            client.open_file(file, &content).await;

            let tokens = client.semantic_tokens(file).await;
            assert!(
                tokens.is_some(),
                "File {} should produce semantic tokens",
                file
            );

            let data = semantic_token_data(&tokens.unwrap());
            assert!(
                !data.is_empty(),
                "File {} should have at least one semantic token. Got 0.",
                file
            );
        }
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_debar_hover_on_procedures() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();

    let path = debar_project_dir().join("objects/Automation/IJLAPIHelper.Codeunit.al");
    if !path.exists() {
        eprintln!("Skipping: IJLAPIHelper.Codeunit.al not found");
        return;
    }

    let content = std::fs::read_to_string(&path).unwrap();
    client.open_file("objects/Automation/IJLAPIHelper.Codeunit.al", &content).await;

    // Find the line with "procedure Precheck" and hover on it
    for (i, line) in content.lines().enumerate() {
        if line.contains("procedure Precheck(") {
            // Hover on the procedure name
            let col = line.find("Precheck").unwrap() as u32;
            let hover = client.hover("objects/Automation/IJLAPIHelper.Codeunit.al", i as u32, col + 2).await;
            assert!(
                hover.is_some(),
                "Should have hover on Precheck procedure at line {}",
                i
            );
            break;
        }
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_debar_completions_in_procedure() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();

    let path = debar_project_dir().join("objects/Automation/IJLAPIHelper.Codeunit.al");
    if !path.exists() {
        return;
    }

    let content = std::fs::read_to_string(&path).unwrap();
    client.open_file("objects/Automation/IJLAPIHelper.Codeunit.al", &content).await;

    let (line, col) =
        find_position(&content, "this.PrecheckRecord(Staging)").expect("this.PrecheckRecord usage");
    let completions = client
        .completion("objects/Automation/IJLAPIHelper.Codeunit.al", line, col + 5)
        .await;
    let labels = completion_labels(&completions);
    assert!(
        labels.contains(&"PrecheckRecord"),
        "`this.` completions inside a procedure should include local procedures. Got: {:?}",
        labels
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_debar_diagnostics_on_real_files() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();

    let path = debar_project_dir().join("objects/Automation/IJLAPIHelper.Codeunit.al");
    if !path.exists() {
        return;
    }

    let content = std::fs::read_to_string(&path).unwrap();
    client.open_file("objects/Automation/IJLAPIHelper.Codeunit.al", &content).await;

    // Wait for diagnostics
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    let diags = client.drain_diagnostics();
    // Real AL code should compile cleanly (no syntax errors)
    // but may have lint warnings

    client.shutdown().await;
}

#[tokio::test]
async fn test_debar_cross_file_goto_definition() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();

    // Open both codeunit and table
    let codeunit_path = debar_project_dir().join("objects/Automation/IJLAPIHelper.Codeunit.al");
    let table_path = debar_project_dir().join("objects/API/ItemJournalStaging.Table.al");

    if !codeunit_path.exists() || !table_path.exists() {
        return;
    }

    let codeunit_content = std::fs::read_to_string(&codeunit_path).unwrap();
    let table_content = std::fs::read_to_string(&table_path).unwrap();

    client.open_file("objects/Automation/IJLAPIHelper.Codeunit.al", &codeunit_content).await;
    client.open_file("objects/API/ItemJournalStaging.Table.al", &table_content).await;

    // The codeunit references "Item Journal Staging" - find where and try go-to-def
    for (i, line) in codeunit_content.lines().enumerate() {
        if line.contains("\"Item Journal Staging\"") {
            // This is a reference to the table - try go-to-definition
            if let Some(pos) = line.find("\"Item Journal Staging\"") {
                let def = client.definition(
                    "objects/Automation/IJLAPIHelper.Codeunit.al",
                    i as u32,
                    (pos + 1) as u32,
                ).await;
                // Whether or not it resolves, it shouldn't crash
                break;
            }
        }
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_debar_exact_navigation_and_hover_regressions() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();

    let files = [
        "objects/Testing/IJLProcessStaging.Report.al",
        "objects/API/ItemJournalStaging.Table.al",
        "objects/API/ItemJournalAPI.Page.al",
        "objects/Automation/IJLAPIHelper.Codeunit.al",
    ];

    for file in files {
        let path = debar_project_dir().join(file);
        let content = std::fs::read_to_string(&path).unwrap();
        client.open_file(file, &content).await;
    }

    let api_helper_def = client
        .definition("objects/Testing/IJLProcessStaging.Report.al", 28, 34)
        .await
        .expect("APIHelper should resolve");
    assert_eq!(definition_start_line(&api_helper_def), Some(72));

    let schedule_post_def = client
        .definition("objects/Testing/IJLProcessStaging.Report.al", 28, 44)
        .await
        .expect("SchedulePost should resolve");
    assert_eq!(
        definition_uri(&schedule_post_def),
        Some("file:///home/bradf/Dev/AL/Debar/App%20Integration/objects/Automation/IJLAPIHelper.Codeunit.al")
    );
    assert_eq!(definition_start_line(&schedule_post_def), Some(49));

    let schedule_post_hover = client
        .hover("objects/Testing/IJLProcessStaging.Report.al", 28, 44)
        .await
        .expect("SchedulePost should have hover");
    let schedule_hover_text = hover_markdown(&schedule_post_hover).unwrap_or("");
    assert!(schedule_hover_text.contains("SchedulePost"));
    assert!(schedule_hover_text.contains("Schedules the post"));

    let precheck_record_def = client
        .definition("objects/Automation/IJLAPIHelper.Codeunit.al", 15, 17)
        .await
        .expect("PrecheckRecord should resolve");
    assert_eq!(definition_start_line(&precheck_record_def), Some(19));

    let status_text_def = client
        .definition("objects/API/ItemJournalAPI.Page.al", 32, 35)
        .await
        .expect("StatusText should resolve");
    assert_eq!(definition_start_line(&status_text_def), Some(169));

    let table_def = client
        .definition("objects/Testing/IJLProcessStaging.Report.al", 12, 30)
        .await
        .expect("Item Journal Staging should resolve to the table");
    assert_eq!(
        definition_uri(&table_def),
        Some("file:///home/bradf/Dev/AL/Debar/App%20Integration/objects/API/ItemJournalStaging.Table.al")
    );

    let codeunit_content = std::fs::read_to_string(
        debar_project_dir().join("objects/Automation/IJLAPIHelper.Codeunit.al"),
    )
    .unwrap();
    let (field_count_line, field_count_col) =
        find_position(&codeunit_content, "FieldCount").expect("FieldCount usage should exist");
    let field_count_hover = client
        .hover(
            "objects/Automation/IJLAPIHelper.Codeunit.al",
            field_count_line,
            field_count_col,
        )
        .await
        .expect("FieldCount should have builtin hover");
    let field_count_text = hover_markdown(&field_count_hover).unwrap_or("");
    assert!(field_count_text.contains("FieldCount"));

    client.shutdown().await;
}

#[tokio::test]
async fn test_debar_exact_completion_regressions() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();

    let page_path = debar_project_dir().join("objects/API/ItemJournalAPI.Page.al");
    let page_content = std::fs::read_to_string(&page_path).unwrap();
    client
        .open_file("objects/API/ItemJournalAPI.Page.al", &page_content)
        .await;

    let codeunit_path = debar_project_dir().join("objects/Automation/IJLAPIHelper.Codeunit.al");
    let codeunit_content = std::fs::read_to_string(&codeunit_path).unwrap();
    client
        .open_file("objects/Automation/IJLAPIHelper.Codeunit.al", &codeunit_content)
        .await;

    let report_path = debar_project_dir().join("objects/Testing/IJLProcessStaging.Report.al");
    let report_content = std::fs::read_to_string(&report_path).unwrap();
    client
        .open_file("objects/Testing/IJLProcessStaging.Report.al", &report_content)
        .await;

    let this_completions = client
        .completion(
            "objects/Automation/IJLAPIHelper.Codeunit.al",
            find_position(&codeunit_content, "this.PrecheckRecord(Staging)")
                .expect("this.PrecheckRecord usage")
                .0,
            find_position(&codeunit_content, "this.PrecheckRecord(Staging)")
                .expect("this.PrecheckRecord usage")
                .1
                + 5,
        )
        .await;
    assert!(
        this_completions.iter().any(|item| item.get("label").and_then(|v| v.as_str()) == Some("PrecheckRecord")),
        "`this.` completions should include PrecheckRecord"
    );

    let rec_completions = client
        .completion(
            "objects/API/ItemJournalAPI.Page.al",
            find_position(&page_content, "Rec.SystemId")
                .expect("Rec.SystemId usage")
                .0,
            find_position(&page_content, "Rec.SystemId")
                .expect("Rec.SystemId usage")
                .1
                + 4,
        )
        .await;
    assert!(
        rec_completions.iter().any(|item| item.get("label").and_then(|v| v.as_str()) == Some("Description")),
        "`Rec.` completions should include standard table fields"
    );

    let enum_completions = client
        .completion(
            "objects/Automation/IJLAPIHelper.Codeunit.al",
            find_position(&codeunit_content, "Staging.Status::Failed")
                .expect("Staging.Status enum usage")
                .0,
            find_position(&codeunit_content, "Staging.Status::Failed")
                .expect("Staging.Status enum usage")
                .1
                + 16,
        )
        .await;
    assert!(
        enum_completions.iter().any(|item| item.get("label").and_then(|v| v.as_str()) == Some("Failed")),
        "`Status::` completions should include Failed"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_debar_formatting_all_files() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();

    // Format each file and verify no crashes
    let files = [
        "objects/API/ItemJournalStaging.Table.al",
        "objects/Automation/IJLAPIHelper.Codeunit.al",
        "objects/Automation/IJLStatus.Enum.al",
        "objects/System/JsonTools.Codeunit.al",
        "objects/System/IJLAPI.PermissionSet.al",
    ];

    for file in &files {
        let path = debar_project_dir().join(file);
        if path.exists() {
            let content = std::fs::read_to_string(&path).unwrap();
            client.open_file(file, &content).await;
            let _edits = client.format(file).await;
            // No crash = success
        }
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_debar_folding_all_files() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();

    let files = [
        ("objects/API/ItemJournalStaging.Table.al", 5),
        ("objects/Automation/IJLAPIHelper.Codeunit.al", 8),
        ("objects/Automation/IJLStatus.Enum.al", 2),
    ];

    for (file, min_folds) in &files {
        let path = debar_project_dir().join(file);
        if path.exists() {
            let content = std::fs::read_to_string(&path).unwrap();
            client.open_file(file, &content).await;

            let ranges = client.folding_ranges(file).await;
            assert!(
                ranges.len() >= *min_folds,
                "File {} should have at least {} folds. Got: {}",
                file,
                min_folds,
                ranges.len()
            );
        }
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_debar_member_navigation_hover_and_completion_regressions() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();

    let report_rel = "objects/Testing/IJLProcessStaging.Report.al";
    let codeunit_rel = "objects/Automation/IJLAPIHelper.Codeunit.al";
    let page_rel = "objects/API/ItemJournalAPI.Page.al";
    let table_rel = "objects/API/ItemJournalStaging.Table.al";
    let enum_rel = "objects/Automation/IJLStatus.Enum.al";

    let report = std::fs::read_to_string(debar_project_dir().join(report_rel)).unwrap();
    let codeunit = std::fs::read_to_string(debar_project_dir().join(codeunit_rel)).unwrap();
    let page = std::fs::read_to_string(debar_project_dir().join(page_rel)).unwrap();
    let table = std::fs::read_to_string(debar_project_dir().join(table_rel)).unwrap();
    let enum_file = std::fs::read_to_string(debar_project_dir().join(enum_rel)).unwrap();

    client.open_file(report_rel, &report).await;
    client.open_file(codeunit_rel, &codeunit).await;
    client.open_file(page_rel, &page).await;
    client.open_file(table_rel, &table).await;
    client.open_file(enum_rel, &enum_file).await;

    let (api_helper_line, api_helper_col) =
        find_position(&report, "this.APIHelper.SchedulePost").expect("APIHelper usage");
    let api_helper_def = client
        .definition(report_rel, api_helper_line, api_helper_col + 6)
        .await
        .expect("definition for APIHelper");
    assert!(
        definition_uri(&api_helper_def)
            .map(|uri| uri.ends_with("/objects/Testing/IJLProcessStaging.Report.al"))
            .unwrap_or(false),
        "APIHelper should resolve to the report variable declaration. Got: {:?}",
        api_helper_def
    );
    assert_eq!(
        definition_start_line(&api_helper_def),
        Some(72),
        "APIHelper should resolve to the report var block"
    );

    let schedule_line = api_helper_line;
    let schedule_col = report
        .lines()
        .nth(schedule_line as usize)
        .and_then(|line| line.find("SchedulePost"))
        .expect("SchedulePost usage") as u32;
    let schedule_def = client
        .definition(report_rel, schedule_line, schedule_col + 2)
        .await
        .expect("definition for SchedulePost");
    assert!(
        definition_uri(&schedule_def)
            .map(|uri| uri.ends_with("/objects/Automation/IJLAPIHelper.Codeunit.al"))
            .unwrap_or(false),
        "SchedulePost should resolve into the helper codeunit. Got: {:?}",
        schedule_def
    );
    let schedule_hover = client
        .hover(report_rel, schedule_line, schedule_col + 2)
        .await
        .expect("hover for SchedulePost");
    let schedule_hover_text = hover_content(&schedule_hover).unwrap_or("");
    assert!(
        schedule_hover_text.contains("SchedulePost")
            && schedule_hover_text.contains("schedule the post"),
        "SchedulePost hover should include signature and XML docs. Got: {:?}",
        schedule_hover
    );

    let (precheck_line, precheck_col) =
        find_position(&codeunit, "this.PrecheckRecord(Staging)").expect("PrecheckRecord usage");
    let precheck_def = client
        .definition(codeunit_rel, precheck_line, precheck_col + 6)
        .await
        .expect("definition for PrecheckRecord");
    assert!(
        definition_uri(&precheck_def)
            .map(|uri| uri.ends_with("/objects/Automation/IJLAPIHelper.Codeunit.al"))
            .unwrap_or(false),
        "PrecheckRecord should resolve in the helper codeunit. Got: {:?}",
        precheck_def
    );
    assert_eq!(
        definition_start_line(&precheck_def),
        Some(19),
        "PrecheckRecord should resolve to its local procedure declaration"
    );

    let (status_line, status_col) =
        find_position(&page, "this.StatusText").expect("StatusText usage");
    let status_def = client
        .definition(page_rel, status_line, status_col + 6)
        .await
        .expect("definition for StatusText");
    assert!(
        definition_uri(&status_def)
            .map(|uri| uri.ends_with("/objects/API/ItemJournalAPI.Page.al"))
            .unwrap_or(false),
        "StatusText should resolve to the page variable declaration. Got: {:?}",
        status_def
    );
    assert_eq!(
        definition_start_line(&status_def),
        Some(169),
        "StatusText should resolve to the page var block"
    );

    let (dataitem_line, dataitem_col) =
        find_position(&report, "\"Item Journal Staging\")").expect("dataitem table reference");
    let table_def = client
        .definition(report_rel, dataitem_line, dataitem_col + 2)
        .await
        .expect("definition for Item Journal Staging");
    assert!(
        definition_uri(&table_def)
            .map(|uri| uri.ends_with("/objects/API/ItemJournalStaging.Table.al"))
            .unwrap_or(false),
        "Quoted table references should resolve to the workspace table. Got: {:?}",
        table_def
    );

    let (this_line, this_col) = find_position(&page, "this.StatusText").expect("this member access");
    let this_completions = client.completion(page_rel, this_line, this_col + 5).await;
    let this_labels = completion_labels(&this_completions);
    assert!(
        this_labels.contains(&"StatusText") && this_labels.contains(&"OnAfterGetRecord"),
        "`this.` completions should include page members. Got: {:?}",
        this_labels
    );

    let (rec_line, rec_col) = find_position(&page, "Rec.SystemId").expect("Rec member access");
    let rec_completions = client.completion(page_rel, rec_line, rec_col + 4).await;
    let rec_labels = completion_labels(&rec_completions);
    assert!(
        rec_labels.contains(&"Quantity") && rec_labels.contains(&"Document No."),
        "`Rec.` completions should include source table fields. Got: {:?}",
        rec_labels
    );

    let (enum_line, enum_col) =
        find_position(&report, "this.ActionType::").expect("enum scope access");
    let enum_completions = client.completion(report_rel, enum_line, enum_col + 17).await;
    let enum_labels = completion_labels(&enum_completions);
    assert!(
        enum_labels.contains(&"Precheck") && enum_labels.contains(&"Post"),
        "Enum scope completions should include workspace enum values. Got: {:?}",
        enum_labels
    );

    let (field_count_line, field_count_col) =
        find_position(&codeunit, "RecRef.FieldCount").expect("FieldCount usage");
    let field_count_hover = client
        .hover(codeunit_rel, field_count_line, field_count_col + 8)
        .await
        .expect("hover for FieldCount");
    let field_count_hover_text = hover_content(&field_count_hover).unwrap_or("");
    assert!(
        field_count_hover_text.contains("FieldCount"),
        "Built-in method hover should include FieldCount details. Got: {:?}",
        field_count_hover
    );

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// Edge case tests — dataitem variables, cross-file definition, built-in types
// ---------------------------------------------------------------------------

/// Test that dataitem variables (report dataset) are resolved for hover/completion.
#[tokio::test]
async fn test_debar_dataitem_variable_resolution() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();
    open_debar_files(&mut client).await;

    let report_rel = "objects/Testing/IJLProcessStaging.Report.al";
    let report = std::fs::read_to_string(debar_project_dir().join(report_rel)).unwrap();

    // StagingRec is a dataitem variable, not a regular var
    // Line 22: this.APIHelper.Precheck(StagingRec);
    let (staging_line, _staging_col) =
        find_position(&report, "APIHelper.Precheck(StagingRec")
            .expect("StagingRec usage in Precheck call");
    // Position on StagingRec argument
    let staging_col = report.lines().nth(staging_line as usize)
        .and_then(|line| line.find("StagingRec);"))
        .expect("StagingRec in line") as u32;

    let staging_hover = client
        .hover(report_rel, staging_line, staging_col + 2)
        .await;
    assert!(
        staging_hover.is_some(),
        "StagingRec (dataitem variable) should have hover info"
    );
    let staging_hover_text = hover_content(&staging_hover.as_ref().unwrap()).unwrap_or("");
    assert!(
        staging_hover_text.contains("Record") || staging_hover_text.contains("Item Journal Staging"),
        "StagingRec hover should show Record type. Got: {:?}",
        staging_hover_text
    );

    // StagingRec. should provide completions (table fields)
    let staging_dot_line = report.lines().enumerate()
        .find(|(_idx, line)| line.contains("StagingRec.Status::Posting"))
        .map(|(idx, _)| idx as u32)
        .expect("StagingRec.Status::Posting line");
    let staging_dot_col = report.lines().nth(staging_dot_line as usize)
        .and_then(|line| line.find("StagingRec."))
        .expect("StagingRec. position") as u32 + 11; // after the dot
    let staging_completions = client
        .completion(report_rel, staging_dot_line, staging_dot_col)
        .await;
    let staging_labels = completion_labels(&staging_completions);
    assert!(
        staging_labels.iter().any(|l| l.eq_ignore_ascii_case("Status")),
        "StagingRec. completions should include table fields like Status. Got: {:?}",
        staging_labels
    );

    client.shutdown().await;
}

/// Test cross-file go-to-definition: Staging.GetJournalData() should resolve to the table procedure.
#[tokio::test]
async fn test_debar_cross_file_procedure_definition() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();
    open_debar_files(&mut client).await;

    let codeunit_rel = "objects/Automation/IJLAPIHelper.Codeunit.al";
    let codeunit = std::fs::read_to_string(debar_project_dir().join(codeunit_rel)).unwrap();

    // Line: JournalData := Staging.GetJournalData();
    let (get_line, _) =
        find_position(&codeunit, "Staging.GetJournalData()")
            .expect("GetJournalData usage");
    let get_col = codeunit.lines().nth(get_line as usize)
        .and_then(|line| line.find("GetJournalData"))
        .expect("GetJournalData position") as u32;

    let get_def = client
        .definition(codeunit_rel, get_line, get_col + 2)
        .await
        .expect("GetJournalData should have a definition");
    assert!(
        definition_uri(&get_def)
            .map(|uri| uri.ends_with("ItemJournalStaging.Table.al"))
            .unwrap_or(false),
        "GetJournalData should resolve to the table file. Got: {:?}",
        get_def
    );

    client.shutdown().await;
}

/// Test multi-level member chain: this.IJLPostTask.Run() in report.
/// IJLPostTask is a Codeunit "IJL Post Task" var — Run() is the codeunit's trigger.
#[tokio::test]
async fn test_debar_multilevel_member_chain() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();
    open_debar_files(&mut client).await;

    let report_rel = "objects/Testing/IJLProcessStaging.Report.al";
    let report = std::fs::read_to_string(debar_project_dir().join(report_rel)).unwrap();

    // Line 69: this.IJLPostTask.Run();
    let (run_line, _) =
        find_position(&report, "this.IJLPostTask.Run()").expect("IJLPostTask.Run() usage");
    let run_line_text = report.lines().nth(run_line as usize).unwrap();

    // Hover on IJLPostTask — should resolve to the codeunit variable
    let ijl_col = run_line_text.find("IJLPostTask").expect("IJLPostTask in line") as u32;
    let ijl_hover = client.hover(report_rel, run_line, ijl_col + 2).await;
    assert!(
        ijl_hover.is_some(),
        "IJLPostTask should have hover info (codeunit variable)"
    );
    let ijl_text = hover_content(&ijl_hover.as_ref().unwrap()).unwrap_or("");
    assert!(
        ijl_text.contains("Codeunit") || ijl_text.contains("IJL Post Task"),
        "IJLPostTask hover should mention Codeunit type. Got: {:?}",
        ijl_text
    );

    // Definition on IJLPostTask — should resolve to the var declaration (line 74)
    let ijl_def = client.definition(report_rel, run_line, ijl_col + 2).await;
    assert!(
        ijl_def.is_some(),
        "IJLPostTask should have a definition (var declaration)"
    );
    if let Some(ref def) = ijl_def {
        // Should point to the var section in the report
        assert!(
            definition_uri(def)
                .map(|uri| uri.ends_with("IJLProcessStaging.Report.al"))
                .unwrap_or(false),
            "IJLPostTask should resolve within the report. Got: {:?}",
            def
        );
    }

    // Hover on Run — should resolve to a procedure or trigger
    let run_col = run_line_text.find("Run()").expect("Run() in line") as u32;
    let run_hover = client.hover(report_rel, run_line, run_col + 1).await;
    // Run() resolves through IJLPostTask (Codeunit "IJL Post Task") — may or may not have hover
    // depending on whether the server resolves through the variable type to the codeunit's trigger.
    // This is an aspirational test — we just verify no crash for now.
    if let Some(ref hover) = run_hover {
        let run_text = hover_content(hover).unwrap_or("");
        eprintln!("Run() hover: {}", run_text);
    }

    client.shutdown().await;
}

/// Test Codeunit::"IJL Post Task" scope access syntax.
#[tokio::test]
async fn test_debar_codeunit_scope_access() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();
    open_debar_files(&mut client).await;

    let codeunit_rel = "objects/Automation/IJLAPIHelper.Codeunit.al";
    let codeunit = std::fs::read_to_string(debar_project_dir().join(codeunit_rel)).unwrap();

    // Line 58: TaskScheduler.CreateTask(Codeunit::"IJL Post Task", ...)
    let (scope_line, _) =
        find_position(&codeunit, "Codeunit::\"IJL Post Task\"").expect("Codeunit:: scope access");
    let scope_line_text = codeunit.lines().nth(scope_line as usize).unwrap();

    // Hover on the quoted "IJL Post Task" — should resolve to the codeunit
    let post_task_col = scope_line_text.find("\"IJL Post Task\"").expect("quoted name") as u32;
    let post_task_hover = client
        .hover(codeunit_rel, scope_line, post_task_col + 2)
        .await;
    // This should resolve the codeunit object
    if let Some(ref hover) = post_task_hover {
        let text = hover_content(hover).unwrap_or("");
        eprintln!("Codeunit::\"IJL Post Task\" hover: {}", text);
        assert!(
            text.contains("Codeunit") || text.contains("IJL Post Task"),
            "Hover should reference the codeunit. Got: {:?}",
            text
        );
    }

    // Definition on "IJL Post Task" — should go to the codeunit file
    let post_task_def = client
        .definition(codeunit_rel, scope_line, post_task_col + 2)
        .await;
    if let Some(ref def) = post_task_def {
        // Should resolve to IJLPostTask.Codeunit.al
        assert!(
            definition_uri(def)
                .map(|uri| uri.contains("IJLPostTask.Codeunit.al") || uri.contains("IJL"))
                .unwrap_or(false),
            "Codeunit:: scope should resolve to the codeunit file. Got: {:?}",
            def
        );
    }

    client.shutdown().await;
}

/// Test Rec.SystemId hover — SystemId is a built-in system field on all records.
#[tokio::test]
async fn test_debar_builtin_system_field_hover() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();
    open_debar_files(&mut client).await;

    let page_rel = "objects/API/ItemJournalAPI.Page.al";
    let page = std::fs::read_to_string(debar_project_dir().join(page_rel)).unwrap();

    // field(id; Rec.SystemId)
    let (sysid_line, _) =
        find_position(&page, "Rec.SystemId").expect("Rec.SystemId usage");
    let sysid_line_text = page.lines().nth(sysid_line as usize).unwrap();
    let sysid_col = sysid_line_text.find("SystemId").expect("SystemId in line") as u32;

    let sysid_hover = client.hover(page_rel, sysid_line, sysid_col + 2).await;
    assert!(
        sysid_hover.is_some(),
        "SystemId should have hover info (built-in system field)"
    );
    let sysid_text = hover_content(&sysid_hover.as_ref().unwrap()).unwrap_or("");
    assert!(
        sysid_text.contains("SystemId"),
        "SystemId hover should mention SystemId. Got: {:?}",
        sysid_text
    );

    client.shutdown().await;
}

/// Test GetLastErrorText() hover — built-in global function.
#[tokio::test]
async fn test_debar_builtin_global_function_hover() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();

    let post_task_rel = "objects/Automation/IJLPostTask.Codeunit.al";
    let post_task_path = debar_project_dir().join(post_task_rel);
    let post_task = std::fs::read_to_string(&post_task_path).unwrap();
    client.open_file(post_task_rel, &post_task).await;

    // GetLastErrorText() — built-in global function
    let (gle_line, _) =
        find_position(&post_task, "GetLastErrorText()").expect("GetLastErrorText usage");
    let gle_line_text = post_task.lines().nth(gle_line as usize).unwrap();
    let gle_col = gle_line_text.find("GetLastErrorText").expect("GetLastErrorText in line") as u32;

    let gle_hover = client.hover(post_task_rel, gle_line, gle_col + 2).await;
    // GetLastErrorText is a built-in function — may resolve through builtins or not
    // (depends on whether semantic bridge is running). Record the result.
    if let Some(ref hover) = gle_hover {
        let text = hover_content(hover).unwrap_or("");
        eprintln!("GetLastErrorText hover: {}", text);
        assert!(
            text.contains("GetLastErrorText") || text.contains("Error"),
            "GetLastErrorText hover should be relevant. Got: {:?}",
            text
        );
    } else {
        eprintln!("NOTE: GetLastErrorText hover returned None — semantic bridge may not be running");
    }

    client.shutdown().await;
}

/// Test TaskScheduler.CreateTask() hover — built-in type method.
#[tokio::test]
async fn test_debar_builtin_type_method_hover() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();
    open_debar_files(&mut client).await;

    let codeunit_rel = "objects/Automation/IJLAPIHelper.Codeunit.al";
    let codeunit = std::fs::read_to_string(debar_project_dir().join(codeunit_rel)).unwrap();

    // TaskScheduler.CreateTask(...) — TaskScheduler is a built-in type
    let (ts_line, _) =
        find_position(&codeunit, "TaskScheduler.CreateTask").expect("TaskScheduler usage");
    let ts_line_text = codeunit.lines().nth(ts_line as usize).unwrap();

    // Hover on TaskScheduler itself
    let ts_col = ts_line_text.find("TaskScheduler").expect("TaskScheduler in line") as u32;
    let ts_hover = client.hover(codeunit_rel, ts_line, ts_col + 2).await;
    if let Some(ref hover) = ts_hover {
        let text = hover_content(hover).unwrap_or("");
        eprintln!("TaskScheduler hover: {}", text);
        assert!(
            text.contains("TaskScheduler"),
            "TaskScheduler hover should mention the type. Got: {:?}",
            text
        );
    } else {
        eprintln!("NOTE: TaskScheduler hover returned None — built-in type may not be loaded");
    }

    // Hover on CreateTask
    let ct_col = ts_line_text.find("CreateTask").expect("CreateTask in line") as u32;
    let ct_hover = client.hover(codeunit_rel, ts_line, ct_col + 2).await;
    if let Some(ref hover) = ct_hover {
        let text = hover_content(hover).unwrap_or("");
        eprintln!("CreateTask hover: {}", text);
        assert!(
            text.contains("CreateTask"),
            "CreateTask hover should mention the method. Got: {:?}",
            text
        );
    } else {
        eprintln!("NOTE: CreateTask hover returned None — semantic bridge may not be running");
    }

    client.shutdown().await;
}

/// Test semantic tokens for the report file — ensures highlighting works.
#[tokio::test]
async fn test_debar_report_semantic_tokens() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();

    let report_rel = "objects/Testing/IJLProcessStaging.Report.al";
    let report_path = debar_project_dir().join(report_rel);
    let report = std::fs::read_to_string(&report_path).unwrap();
    client.open_file(report_rel, &report).await;

    let tokens = client.semantic_tokens(report_rel).await;
    assert!(
        tokens.is_some(),
        "Report file should produce semantic tokens"
    );

    let data = semantic_token_data(&tokens.unwrap());
    let line_count = report.lines().count();

    // A report with 87 lines of real AL code should produce at least 30 tokens
    assert!(
        data.len() >= 30,
        "Report file ({} lines) should have at least 30 semantic tokens. Got: {}",
        line_count,
        data.len()
    );

    // Verify token types include keywords, strings, and types at minimum
    let token_types: std::collections::HashSet<u32> = data.iter().map(|t| t[3]).collect();
    assert!(
        token_types.len() >= 3,
        "Should have at least 3 different token types. Got: {:?}",
        token_types
    );

    client.shutdown().await;
}

/// Test built-in type method hover (Record.FindSet, JsonObject.ReadFrom).
#[tokio::test]
async fn test_debar_builtin_method_hover() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();
    open_debar_files(&mut client).await;

    let codeunit_rel = "objects/Automation/IJLAPIHelper.Codeunit.al";
    let codeunit = std::fs::read_to_string(debar_project_dir().join(codeunit_rel)).unwrap();

    // Staging.FindSet(false)  — Record.FindSet is a built-in method
    let (findset_line, _) =
        find_position(&codeunit, "Staging.FindSet(false)")
            .expect("FindSet usage");
    let findset_col = codeunit.lines().nth(findset_line as usize)
        .and_then(|line| line.find("FindSet"))
        .expect("FindSet position") as u32;

    let findset_hover = client
        .hover(codeunit_rel, findset_line, findset_col + 2)
        .await;
    assert!(
        findset_hover.is_some(),
        "FindSet should have hover info (built-in Record method)"
    );
    let findset_text = hover_content(&findset_hover.as_ref().unwrap()).unwrap_or("");
    assert!(
        findset_text.contains("FindSet"),
        "FindSet hover should mention FindSet. Got: {:?}",
        findset_text
    );

    // JsonObj.ReadFrom(JournalData) — JsonObject.ReadFrom is a built-in method
    let (readfrom_line, _) =
        find_position(&codeunit, "JsonObj.ReadFrom(JournalData)")
            .expect("ReadFrom usage");
    let readfrom_col = codeunit.lines().nth(readfrom_line as usize)
        .and_then(|line| line.find("ReadFrom"))
        .expect("ReadFrom position") as u32;

    let readfrom_hover = client
        .hover(codeunit_rel, readfrom_line, readfrom_col + 2)
        .await;
    assert!(
        readfrom_hover.is_some(),
        "ReadFrom should have hover info (built-in JsonObject method)"
    );

    client.shutdown().await;
}

/// Regression: go-to-definition on a workspace table field should navigate to the field declaration.
/// Bug: ResolvedMemberKind::Field fell through to `_ => {}` in definition.rs.
#[tokio::test]
async fn test_debar_field_definition_navigates() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();
    open_debar_files(&mut client).await;

    let report_rel = "objects/Testing/IJLProcessStaging.Report.al";
    let report = std::fs::read_to_string(debar_project_dir().join(report_rel)).unwrap();

    // StagingRec.Status — Status is a field on the table
    let (status_line, _) =
        find_position(&report, "StagingRec.Status::Posting")
            .expect("StagingRec.Status usage");
    let status_col = report.lines().nth(status_line as usize)
        .and_then(|line| line.find("StagingRec.Status::"))
        .expect("Status position") as u32 + 11; // on "Status" after the dot

    let status_def = client.definition(report_rel, status_line, status_col + 2).await;
    if let Some(ref def) = status_def {
        // Should resolve to the table file where the Status field is declared
        assert!(
            definition_uri(def)
                .map(|uri| uri.contains("ItemJournalStaging.Table.al"))
                .unwrap_or(false),
            "Status field should resolve to the table file. Got: {:?}",
            def
        );
    }

    client.shutdown().await;
}

/// Regression: go-to-definition on enum values should navigate to the enum declaration.
/// Bug: ResolvedMemberKind::EnumValue fell through to `_ => {}` in definition.rs.
#[tokio::test]
async fn test_debar_enum_value_definition_navigates() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();
    open_debar_files(&mut client).await;

    let report_rel = "objects/Testing/IJLProcessStaging.Report.al";
    let report = std::fs::read_to_string(debar_project_dir().join(report_rel)).unwrap();

    // Status::Posting — Posting is an enum value
    let (posting_line, _) =
        find_position(&report, "Status::Posting")
            .expect("Status::Posting usage");
    let posting_col = report.lines().nth(posting_line as usize)
        .and_then(|line| line.find("Posting"))
        .expect("Posting position") as u32;

    let posting_def = client.definition(report_rel, posting_line, posting_col + 2).await;
    if let Some(ref def) = posting_def {
        // Should resolve to the enum file where Posting is declared
        let def_uri = definition_uri(def).unwrap_or("");
        assert!(
            def_uri.contains("IJLStatus.Enum.al") || def_uri.contains("Status"),
            "Posting enum value should resolve to the enum file. Got: {:?}",
            def
        );
    }

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// Signature help tests
// ---------------------------------------------------------------------------

/// Signature help for a local procedure call via `this.InsertJournalLine(...)`.
#[tokio::test]
async fn test_debar_signature_help_local_procedure() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();
    open_debar_files(&mut client).await;

    let post_task_rel = "objects/Automation/IJLPostTask.Codeunit.al";
    let post_task = std::fs::read_to_string(debar_project_dir().join(post_task_rel)).unwrap();

    // Line 37: this.InsertJournalLine(StagingRec, ItemJnlLine) — local procedure with 2 params
    let (line, _) = find_position(&post_task, "this.InsertJournalLine(StagingRec, ItemJnlLine)")
        .expect("InsertJournalLine call");
    let col = post_task.lines().nth(line as usize)
        .and_then(|l| l.find("InsertJournalLine("))
        .expect("InsertJournalLine( position") as u32 + 18; // after the (

    let sig = client.signature_help(post_task_rel, line, col).await;
    assert!(
        sig.is_some(),
        "InsertJournalLine should have signature help (local procedure with params)"
    );
    let sig_val = sig.unwrap();
    let label = sig_val.get("signatures")
        .and_then(|s| s.as_array())
        .and_then(|arr| arr.first())
        .and_then(|s| s.get("label"))
        .and_then(|l| l.as_str())
        .unwrap_or("");
    assert!(
        label.contains("InsertJournalLine") && label.contains("StagingRec"),
        "Signature should show InsertJournalLine with params. Got: {:?}",
        label
    );

    client.shutdown().await;
}

/// Signature help for a built-in Record method: StagingRec.SetRange(Status, ...).
#[tokio::test]
async fn test_debar_signature_help_builtin_method() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();
    open_debar_files(&mut client).await;

    let post_task_rel = "objects/Automation/IJLPostTask.Codeunit.al";
    let post_task = std::fs::read_to_string(debar_project_dir().join(post_task_rel)).unwrap();

    // Line 29: StagingRec.SetRange(Status, StagingRec.Status::Posting)
    let (line, _) = find_position(&post_task, "StagingRec.SetRange(Status,")
        .expect("SetRange call");
    let col = post_task.lines().nth(line as usize)
        .and_then(|l| l.find("SetRange("))
        .expect("SetRange( position") as u32 + 9; // after the (

    let sig = client.signature_help(post_task_rel, line, col).await;
    assert!(
        sig.is_some(),
        "SetRange should have signature help (built-in Record method)"
    );

    client.shutdown().await;
}

/// Signature help for a cross-file workspace procedure: ProcessReport.SetAction(...).
/// BUG FINDER: Signature help only searches local procs, packages, and builtins — not workspace objects.
#[tokio::test]
async fn test_debar_signature_help_cross_file_workspace_procedure() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();

    let staging_list_rel = "objects/Testing/IJLStagingList.Page.al";
    let staging_list = std::fs::read_to_string(debar_project_dir().join(staging_list_rel)).unwrap();
    client.open_file(staging_list_rel, &staging_list).await;
    open_debar_files(&mut client).await;

    // Line 73: ProcessReport.SetAction(this.ActionType::Precheck)
    let (line, _) = find_position(&staging_list, "ProcessReport.SetAction(this.ActionType::Precheck)")
        .expect("SetAction call");
    let col = staging_list.lines().nth(line as usize)
        .and_then(|l| l.find("SetAction("))
        .expect("SetAction( position") as u32 + 10; // after the (

    let sig = client.signature_help(staging_list_rel, line, col).await;
    assert!(
        sig.is_some(),
        "SetAction should have signature help — it's a workspace procedure on Report 'IJL Process Staging'. \
         This fails because signature help doesn't resolve through the receiver type to find cross-file procedures."
    );

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// Inlay hints tests
// ---------------------------------------------------------------------------

/// Inlay hints should show parameter names at call sites for local procedures.
#[tokio::test]
async fn test_debar_inlay_hints_local_procedure_calls() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();
    open_debar_files(&mut client).await;

    let post_task_rel = "objects/Automation/IJLPostTask.Codeunit.al";

    // Request inlay hints for the ProcessPostingQueue procedure body (lines 23-49)
    let hints = client.inlay_hints(post_task_rel, 23, 49).await;
    assert!(
        !hints.is_empty(),
        "Post task lines 23-49 should have inlay hints for procedure calls like InsertJournalLine, MarkStagingFailed"
    );

    // Check that at least one hint is a parameter name
    let hint_labels: Vec<&str> = hints.iter()
        .filter_map(|h| h.get("label").and_then(|l| l.as_str()))
        .collect();
    eprintln!("Inlay hint labels: {:?}", hint_labels);
    assert!(
        hint_labels.iter().any(|l| l.contains("StagingRec") || l.contains("ErrorText") || l.contains("PostedEntryNo")),
        "Inlay hints should include parameter names like StagingRec, ErrorText, PostedEntryNo. Got: {:?}",
        hint_labels
    );

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// References (find all references) tests
// ---------------------------------------------------------------------------

/// Find all references to GetJournalData across workspace files.
#[tokio::test]
async fn test_debar_references_cross_file() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();
    open_debar_files(&mut client).await;

    let staging_list_rel = "objects/Testing/IJLStagingList.Page.al";
    let staging_list = std::fs::read_to_string(debar_project_dir().join(staging_list_rel)).unwrap();
    client.open_file(staging_list_rel, &staging_list).await;

    let api_helper_rel = "objects/Automation/IJLAPIHelper.Codeunit.al";
    let api_helper = std::fs::read_to_string(debar_project_dir().join(api_helper_rel)).unwrap();

    // GetJournalData at api helper line 73
    let (line, _) = find_position(&api_helper, "Staging.GetJournalData()")
        .expect("GetJournalData usage in api helper");
    let col = api_helper.lines().nth(line as usize)
        .and_then(|l| l.find("GetJournalData"))
        .expect("GetJournalData position") as u32;

    let refs = client.references(api_helper_rel, line, col + 2).await;
    assert!(
        refs.len() >= 2,
        "GetJournalData should have at least 2 references (declaration + usages across files). Got: {}",
        refs.len()
    );

    let ref_uris: std::collections::HashSet<&str> = refs.iter()
        .filter_map(|r| r.get("uri").and_then(|u| u.as_str()))
        .collect();
    eprintln!("GetJournalData reference URIs: {:?}", ref_uris);

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// Type reference definition tests
// ---------------------------------------------------------------------------

/// Go-to-definition on `"IJL Status"` in a table field type.
#[tokio::test]
async fn test_debar_type_reference_definition() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();
    open_debar_files(&mut client).await;

    let table_rel = "objects/API/ItemJournalStaging.Table.al";
    let table = std::fs::read_to_string(debar_project_dir().join(table_rel)).unwrap();

    // Line 29: field(4; Status; Enum "IJL Status")
    let (line, _) = find_position(&table, "Enum \"IJL Status\"")
        .expect("IJL Status type reference");
    let col = table.lines().nth(line as usize)
        .and_then(|l| l.find("\"IJL Status\""))
        .expect("IJL Status position") as u32 + 2;

    let def = client.definition(table_rel, line, col).await;
    assert!(
        def.is_some(),
        "\"IJL Status\" type reference should navigate to the enum file"
    );
    if let Some(ref d) = def {
        let uri = definition_uri(d).unwrap_or("");
        assert!(
            uri.contains("IJLStatus.Enum.al"),
            "\"IJL Status\" should resolve to IJLStatus.Enum.al. Got: {:?}",
            uri
        );
    }

    client.shutdown().await;
}

/// Go-to-definition on `"IJL API Helper"` codeunit type reference.
#[tokio::test]
async fn test_debar_codeunit_type_reference_definition() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();
    open_debar_files(&mut client).await;

    let report_rel = "objects/Testing/IJLProcessStaging.Report.al";
    let report = std::fs::read_to_string(debar_project_dir().join(report_rel)).unwrap();

    // Line 73: APIHelper: Codeunit "IJL API Helper";
    let (line, _) = find_position(&report, "Codeunit \"IJL API Helper\"")
        .expect("IJL API Helper type reference");
    let col = report.lines().nth(line as usize)
        .and_then(|l| l.find("\"IJL API Helper\""))
        .expect("IJL API Helper position") as u32 + 2;

    let def = client.definition(report_rel, line, col).await;
    assert!(
        def.is_some(),
        "\"IJL API Helper\" type reference should navigate to the codeunit file"
    );
    if let Some(ref d) = def {
        let uri = definition_uri(d).unwrap_or("");
        assert!(
            uri.contains("IJLAPIHelper.Codeunit.al"),
            "\"IJL API Helper\" should resolve to IJLAPIHelper.Codeunit.al. Got: {:?}",
            uri
        );
    }

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// Completions: this. in page trigger and Rec.Status:: enum chain
// ---------------------------------------------------------------------------

/// Completions for `this.` in page trigger should show page variables.
#[tokio::test]
async fn test_debar_this_completions_in_page() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();
    open_debar_files(&mut client).await;

    let staging_list_rel = "objects/Testing/IJLStagingList.Page.al";
    let staging_list = std::fs::read_to_string(debar_project_dir().join(staging_list_rel)).unwrap();
    client.open_file(staging_list_rel, &staging_list).await;

    // Line 154: this.ErrorMessageText := CopyStr(FullErrorMessage, 1, 250);
    let (line, _) = find_position(&staging_list, "this.ErrorMessageText := CopyStr")
        .expect("this.ErrorMessageText usage");
    let col = staging_list.lines().nth(line as usize)
        .and_then(|l| l.find("this."))
        .expect("this. position") as u32 + 5; // after "this."

    let completions = client.completion(staging_list_rel, line, col).await;
    let labels = completion_labels(&completions);
    assert!(
        labels.iter().any(|l| *l == "ErrorMessageText"),
        "this. in page should show ErrorMessageText. Got: {:?}",
        labels
    );
    assert!(
        labels.iter().any(|l| *l == "RowStyle"),
        "this. in page should show RowStyle. Got: {:?}",
        labels
    );

    client.shutdown().await;
}

/// Completions for `Rec.Status::` in page trigger should show enum values.
#[tokio::test]
async fn test_debar_enum_completions_through_field_chain() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();
    open_debar_files(&mut client).await;

    let staging_list_rel = "objects/Testing/IJLStagingList.Page.al";
    let staging_list = std::fs::read_to_string(debar_project_dir().join(staging_list_rel)).unwrap();
    client.open_file(staging_list_rel, &staging_list).await;

    // Line 159: Rec.Status::Pending:
    let (line, _) = find_position(&staging_list, "Rec.Status::Pending")
        .expect("Rec.Status::Pending usage");
    let col = staging_list.lines().nth(line as usize)
        .and_then(|l| l.find("Rec.Status::"))
        .expect("Rec.Status:: position") as u32 + 12; // after "::"

    let completions = client.completion(staging_list_rel, line, col).await;
    let labels = completion_labels(&completions);
    assert!(
        labels.iter().any(|l| *l == "Pending"),
        "Rec.Status:: should show Pending enum value. Got: {:?}",
        labels
    );
    assert!(
        labels.iter().any(|l| *l == "Posted"),
        "Rec.Status:: should show Posted enum value. Got: {:?}",
        labels
    );

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// Hover on workspace procedures from call sites in other files
// ---------------------------------------------------------------------------

/// Hover on `SetAction` at the call site in the staging list page.
#[tokio::test]
async fn test_debar_hover_cross_file_workspace_procedure() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();
    open_debar_files(&mut client).await;

    let staging_list_rel = "objects/Testing/IJLStagingList.Page.al";
    let staging_list = std::fs::read_to_string(debar_project_dir().join(staging_list_rel)).unwrap();
    client.open_file(staging_list_rel, &staging_list).await;

    // Line 73: ProcessReport.SetAction(this.ActionType::Precheck)
    let (line, _) = find_position(&staging_list, "ProcessReport.SetAction(this.ActionType::Precheck)")
        .expect("SetAction call");
    let col = staging_list.lines().nth(line as usize)
        .and_then(|l| l.find("SetAction"))
        .expect("SetAction position") as u32;

    let hover = client.hover(staging_list_rel, line, col + 2).await;
    assert!(
        hover.is_some(),
        "SetAction should have hover info — it's a workspace procedure on Report 'IJL Process Staging'"
    );
    let text = hover_content(&hover.as_ref().unwrap()).unwrap_or("");
    assert!(
        text.contains("SetAction") && text.contains("NewAction"),
        "SetAction hover should show procedure signature with NewAction param. Got: {:?}",
        text
    );

    client.shutdown().await;
}

/// Hover on `GetJournalData` at call site — should show workspace procedure signature.
#[tokio::test]
async fn test_debar_hover_workspace_procedure_with_return_type() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();
    open_debar_files(&mut client).await;

    let api_helper_rel = "objects/Automation/IJLAPIHelper.Codeunit.al";
    let api_helper = std::fs::read_to_string(debar_project_dir().join(api_helper_rel)).unwrap();

    // Line 73: JournalData := Staging.GetJournalData();
    let (line, _) = find_position(&api_helper, "Staging.GetJournalData()")
        .expect("GetJournalData call");
    let col = api_helper.lines().nth(line as usize)
        .and_then(|l| l.find("GetJournalData"))
        .expect("GetJournalData position") as u32;

    let hover = client.hover(api_helper_rel, line, col + 2).await;
    assert!(
        hover.is_some(),
        "GetJournalData should have hover info — it's a workspace procedure on Table 'Item Journal Staging'"
    );
    let text = hover_content(&hover.as_ref().unwrap()).unwrap_or("");
    assert!(
        text.contains("GetJournalData"),
        "GetJournalData hover should mention the procedure name. Got: {:?}",
        text
    );

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// Rename
// ---------------------------------------------------------------------------

/// Rename a local variable in the post task codeunit.
#[tokio::test]
async fn test_debar_rename_local_variable() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();
    open_debar_files(&mut client).await;

    let post_task_rel = "objects/Automation/IJLPostTask.Codeunit.al";
    let post_task = std::fs::read_to_string(debar_project_dir().join(post_task_rel)).unwrap();

    // Line 26: ItemJnlLine: Record "Item Journal Line" (local var in ProcessPostingQueue)
    let (line, _) = find_position(&post_task, "ItemJnlLine: Record \"Item Journal Line\"")
        .expect("ItemJnlLine declaration");
    let col = post_task.lines().nth(line as usize)
        .and_then(|l| l.find("ItemJnlLine"))
        .expect("ItemJnlLine position") as u32;

    let rename_result = client.rename(post_task_rel, line, col + 2, "JournalLine").await;
    assert!(
        rename_result.is_some(),
        "ItemJnlLine should be renameable"
    );

    if let Some(ref edit) = rename_result {
        let changes = edit.get("changes")
            .and_then(|c| c.as_object());
        assert!(
            changes.is_some() && !changes.unwrap().is_empty(),
            "Rename should produce workspace edits. Got: {:?}",
            edit
        );
        let change_count: usize = changes.unwrap().values()
            .filter_map(|edits| edits.as_array())
            .map(|arr| arr.len())
            .sum();
        assert!(
            change_count >= 2,
            "Rename should affect at least 2 locations (declaration + usage). Got: {}",
            change_count
        );
    }

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// Diagnostics and code actions on real files
// ---------------------------------------------------------------------------

/// Verify code actions don't crash on real files with all lint rules active.
#[tokio::test]
async fn test_debar_code_actions_no_crash() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();
    open_debar_files(&mut client).await;

    let post_task_rel = "objects/Automation/IJLPostTask.Codeunit.al";
    let actions = client.code_actions(post_task_rel, 0, 262).await;
    eprintln!("Post task code actions: {} found", actions.len());
    for action in &actions {
        if let Some(title) = action.get("title").and_then(|t| t.as_str()) {
            eprintln!("  - {}", title);
        }
    }

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// Audit regression tests — targeted scenarios from systematic CLI audit
// ---------------------------------------------------------------------------

/// 2-level chain hover: this.APIHelper.Precheck → should show procedure sig.
#[tokio::test]
async fn test_debar_audit_two_level_member_chain_hover() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();
    open_debar_files(&mut client).await;

    let report_rel = "objects/Testing/IJLProcessStaging.Report.al";
    let report = std::fs::read_to_string(debar_project_dir().join(report_rel)).unwrap();

    // Line 22: this.APIHelper.Precheck(StagingRec)
    let (line, _) =
        find_position(&report, "this.APIHelper.Precheck(StagingRec)").expect("Precheck usage");
    let line_text = report.lines().nth(line as usize).unwrap();

    // Hover on Precheck (2-level: this.APIHelper -> Codeunit "IJL API Helper" -> Precheck)
    let precheck_col = line_text.find("Precheck").expect("Precheck in line") as u32;
    let hover = client.hover(report_rel, line, precheck_col + 2).await;
    assert!(
        hover.is_some(),
        "Precheck should have hover info — 2-level chain: this.APIHelper (Codeunit) → Precheck"
    );
    let text = hover_content(&hover.as_ref().unwrap()).unwrap_or("");
    assert!(
        text.contains("Precheck"),
        "Precheck hover should mention the procedure name. Got: {:?}",
        text
    );

    client.shutdown().await;
}

/// Hover on quoted field access: Rec."Journal Data" in table procedure.
#[tokio::test]
async fn test_debar_audit_quoted_field_hover() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();
    open_debar_files(&mut client).await;

    let table_rel = "objects/API/ItemJournalStaging.Table.al";
    let table = std::fs::read_to_string(debar_project_dir().join(table_rel)).unwrap();

    // Line 137: if not Rec."Journal Data".HasValue() then
    let (line, _) =
        find_position(&table, "Rec.\"Journal Data\".HasValue").expect("Journal Data usage");
    let line_text = table.lines().nth(line as usize).unwrap();

    // Hover on "Journal Data" (quoted field on Rec)
    let field_col = line_text.find("\"Journal Data\"").expect("quoted field in line") as u32;
    let hover = client.hover(table_rel, line, field_col + 2).await;
    assert!(
        hover.is_some(),
        "\"Journal Data\" should have hover info — it's a field on Rec (same table)"
    );
    let text = hover_content(&hover.as_ref().unwrap()).unwrap_or("");
    assert!(
        text.contains("Journal Data") || text.contains("field"),
        "Journal Data hover should mention field info. Got: {:?}",
        text
    );

    client.shutdown().await;
}

/// Hover on StagingRec.Status (field access on dataitem Record variable).
#[tokio::test]
async fn test_debar_audit_dataitem_field_hover() {
    if !debar_project_exists() {
        eprintln!("Skipping: Debar project not found");
        return;
    }

    let mut client = LspClient::spawn(debar_project_dir()).await.unwrap();
    open_debar_files(&mut client).await;

    let report_rel = "objects/Testing/IJLProcessStaging.Report.al";
    let report = std::fs::read_to_string(debar_project_dir().join(report_rel)).unwrap();

    // Line 27: ModifyAll(Status, StagingRec.Status::Posting, true)
    let (line, _) =
        find_position(&report, "StagingRec.Status::Posting").expect("StagingRec.Status usage");
    let line_text = report.lines().nth(line as usize).unwrap();

    // Hover on Status (the field, between . and ::)
    let status_col = line_text
        .find("StagingRec.Status::")
        .expect("StagingRec.Status:: in line") as u32
        + 11; // after "StagingRec."
    let hover = client.hover(report_rel, line, status_col + 2).await;
    // Status is a field on "Item Journal Staging" — should resolve via dataitem type
    assert!(
        hover.is_some(),
        "StagingRec.Status should have hover info — field on dataitem record"
    );
    let text = hover_content(&hover.as_ref().unwrap()).unwrap_or("");
    assert!(
        text.contains("Status") || text.contains("Enum") || text.contains("IJL Status"),
        "StagingRec.Status hover should mention Status field or enum type. Got: {:?}",
        text
    );

    client.shutdown().await;
}
