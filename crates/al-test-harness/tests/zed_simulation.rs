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
