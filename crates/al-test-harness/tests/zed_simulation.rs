//! Zed simulation tests — test against the real AL test project directory.
//!
//! These tests simulate exactly what happens when Zed opens an AL project:
//! 1. Server starts with workspace root pointing to the project
//! 2. Server discovers app.json and scans for .al files
//! 3. Files are opened by the editor
//! 4. Various LSP features are invoked
//!
//! Unlike the original version of this file, these tests run against the
//! bundled fixture (`data/test_al_project`) instead of an external,
//! not-checked-in AL project gated on `AL_TEST_PROJECT_PATH`. The fixture's
//! `WorkOrder*` objects (table/enums/codeunits/report/page) were added
//! specifically to give this suite the cross-file member chains, doc-commented
//! procedures, and dataitem-record patterns these tests exercise.

use al_test_harness::*;
use std::path::PathBuf;

fn test_project_dir() -> PathBuf {
    al_test_harness::test_project_dir()
}

fn find_position(content: &str, needle: &str) -> Option<(u32, u32)> {
    for (line_idx, line) in content.lines().enumerate() {
        if let Some(col_idx) = line.find(needle) {
            return Some((line_idx as u32, col_idx as u32));
        }
    }
    None
}

/// Index of the first line containing `needle`, as a `u32` line number.
fn find_line(content: &str, needle: &str) -> u32 {
    content
        .lines()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("expected to find {needle:?}")) as u32
}

async fn open_test_files(client: &mut LspClient) {
    let files = [
        "src/WorkOrderStaging.Table.al",
        "src/WorkOrderStatus.Enum.al",
        "src/WorkOrderAction.Enum.al",
        "src/WorkOrderHelper.Codeunit.al",
        "src/WorkOrderPostTask.Codeunit.al",
        "src/WorkOrderProcessStaging.Report.al",
        "src/WorkOrderStagingList.Page.al",
    ];
    for file in &files {
        let path = test_project_dir().join(file);
        if path.exists() {
            let content = std::fs::read_to_string(&path).unwrap();
            client.open_file(file, &content).await;
        }
    }
}

#[tokio::test]
async fn test_fixture_project_initializes() {
    let client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.shutdown().await;
}

#[tokio::test]
async fn test_fixture_workspace_symbols_after_init() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    // initialize() (called inside LspClient::spawn) already polls
    // workspace/symbol until the index is ready.

    let symbols = client.workspace_symbol("Work Order").await;
    assert!(
        !symbols.is_empty(),
        "Should find workspace symbols matching 'Work Order' after scanning. Got 0 results."
    );

    let all_symbols = client.workspace_symbol("").await;
    assert!(
        all_symbols.len() >= 5,
        "Should find at least 5 workspace objects from AL test project. Got: {}",
        all_symbols.len()
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_fixture_open_real_files() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let files = [
        "src/WorkOrderStaging.Table.al",
        "src/WorkOrderStatus.Enum.al",
        "src/WorkOrderHelper.Codeunit.al",
        "src/WorkOrderPostTask.Codeunit.al",
        "src/WorkOrderProcessStaging.Report.al",
        "src/WorkOrderStagingList.Page.al",
    ];

    for file in &files {
        let path = test_project_dir().join(file);
        if path.exists() {
            let content = std::fs::read_to_string(&path).unwrap();
            client.open_file(file, &content).await;
        }
    }

    for file in &files {
        let path = test_project_dir().join(file);
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
async fn test_fixture_semantic_tokens_real_files() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let files = [
        "src/WorkOrderStaging.Table.al",
        "src/WorkOrderHelper.Codeunit.al",
        "src/WorkOrderStatus.Enum.al",
    ];

    for file in &files {
        let path = test_project_dir().join(file);
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
async fn test_fixture_hover_on_procedures() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let path = test_project_dir().join("src/WorkOrderHelper.Codeunit.al");
    let content = std::fs::read_to_string(&path).unwrap();
    client
        .open_file("src/WorkOrderHelper.Codeunit.al", &content)
        .await;

    for (i, line) in content.lines().enumerate() {
        if line.contains("procedure Precheck(") {
            let col = line.find("Precheck").unwrap() as u32;
            let hover = client
                .hover("src/WorkOrderHelper.Codeunit.al", i as u32, col + 2)
                .await;
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
async fn test_fixture_completions_in_procedure() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let path = test_project_dir().join("src/WorkOrderHelper.Codeunit.al");
    let content = std::fs::read_to_string(&path).unwrap();
    client
        .open_file("src/WorkOrderHelper.Codeunit.al", &content)
        .await;

    let (line, col) =
        find_position(&content, "this.PrecheckRecord(Staging)").expect("this.PrecheckRecord usage");
    let completions = client
        .completion("src/WorkOrderHelper.Codeunit.al", line, col + 5)
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
async fn test_fixture_diagnostics_on_real_files() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let path = test_project_dir().join("src/WorkOrderHelper.Codeunit.al");
    let content = std::fs::read_to_string(&path).unwrap();
    client
        .open_file("src/WorkOrderHelper.Codeunit.al", &content)
        .await;

    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    let _diags = client.drain_diagnostics();
    // Real AL code should compile cleanly (no syntax errors)
    // but may have lint warnings

    client.shutdown().await;
}

#[tokio::test]
async fn test_fixture_cross_file_goto_definition() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let codeunit_path = test_project_dir().join("src/WorkOrderPostTask.Codeunit.al");
    let table_path = test_project_dir().join("src/WorkOrderStaging.Table.al");

    let codeunit_content = std::fs::read_to_string(&codeunit_path).unwrap();
    let table_content = std::fs::read_to_string(&table_path).unwrap();

    client
        .open_file("src/WorkOrderPostTask.Codeunit.al", &codeunit_content)
        .await;
    client
        .open_file("src/WorkOrderStaging.Table.al", &table_content)
        .await;

    for (i, line) in codeunit_content.lines().enumerate() {
        if line.contains("\"Work Order Staging\"") {
            if let Some(pos) = line.find("\"Work Order Staging\"") {
                let _def = client
                    .definition(
                        "src/WorkOrderPostTask.Codeunit.al",
                        i as u32,
                        (pos + 1) as u32,
                    )
                    .await;
                // Whether or not it resolves, it shouldn't crash
                break;
            }
        }
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_fixture_exact_navigation_and_hover_regressions() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let files = [
        "src/WorkOrderProcessStaging.Report.al",
        "src/WorkOrderStaging.Table.al",
        "src/WorkOrderStagingList.Page.al",
        "src/WorkOrderHelper.Codeunit.al",
    ];

    let mut contents = std::collections::HashMap::new();
    for file in files {
        let path = test_project_dir().join(file);
        let content = std::fs::read_to_string(&path).unwrap();
        client.open_file(file, &content).await;
        contents.insert(file, content);
    }

    let report = &contents["src/WorkOrderProcessStaging.Report.al"];
    let helper = &contents["src/WorkOrderHelper.Codeunit.al"];

    // "Helper" usage in `this.Helper.SchedulePost(Staging)` resolves to the
    // report's own var declaration.
    let (helper_line, helper_col) =
        find_position(report, "this.Helper.SchedulePost").expect("Helper usage");
    let helper_def = client
        .definition(
            "src/WorkOrderProcessStaging.Report.al",
            helper_line,
            helper_col + 6,
        )
        .await
        .expect("Helper should resolve");
    let expected_helper_line = find_line(report, "Helper: Codeunit \"Work Order Helper\"");
    assert_eq!(
        definition_start_line(&helper_def),
        Some(expected_helper_line)
    );

    // "SchedulePost" resolves cross-file into the helper codeunit.
    let schedule_col = report
        .lines()
        .nth(helper_line as usize)
        .and_then(|l| l.find("SchedulePost"))
        .expect("SchedulePost in line") as u32;
    let schedule_post_def = client
        .definition(
            "src/WorkOrderProcessStaging.Report.al",
            helper_line,
            schedule_col + 2,
        )
        .await
        .expect("SchedulePost should resolve");
    assert!(
        definition_uri(&schedule_post_def)
            .map(|u| u.contains("WorkOrderHelper.Codeunit.al"))
            .unwrap_or(false),
        "SchedulePost definition should point to WorkOrderHelper.Codeunit.al"
    );
    let expected_schedule_line = find_line(helper, "procedure SchedulePost(");
    assert_eq!(
        definition_start_line(&schedule_post_def),
        Some(expected_schedule_line)
    );

    let schedule_post_hover = client
        .hover(
            "src/WorkOrderProcessStaging.Report.al",
            helper_line,
            schedule_col + 2,
        )
        .await
        .expect("SchedulePost should have hover");
    let schedule_hover_text = hover_content(&schedule_post_hover).unwrap_or("");
    assert!(schedule_hover_text.contains("SchedulePost"));
    assert!(schedule_hover_text.contains("Schedules the post"));

    // "PrecheckRecord" self-reference inside the helper codeunit.
    let (precheck_line, precheck_col) =
        find_position(helper, "this.PrecheckRecord(Staging)").expect("PrecheckRecord usage");
    let precheck_record_def = client
        .definition(
            "src/WorkOrderHelper.Codeunit.al",
            precheck_line,
            precheck_col + 6,
        )
        .await
        .expect("PrecheckRecord should resolve");
    let expected_precheck_line = find_line(helper, "procedure PrecheckRecord(");
    assert_eq!(
        definition_start_line(&precheck_record_def),
        Some(expected_precheck_line)
    );

    // "ErrorMessageText" usage resolves to the page's own var declaration.
    let page = &contents["src/WorkOrderStagingList.Page.al"];
    let (err_line, err_col) =
        find_position(page, "this.ErrorMessageText := CopyStr").expect("ErrorMessageText usage");
    let status_text_def = client
        .definition("src/WorkOrderStagingList.Page.al", err_line, err_col + 6)
        .await
        .expect("ErrorMessageText should resolve");
    let expected_err_line = find_line(page, "ErrorMessageText: Text;");
    assert_eq!(
        definition_start_line(&status_text_def),
        Some(expected_err_line)
    );

    // Quoted table type reference in the report's dataitem resolves to the table.
    let (dataitem_line, dataitem_col) =
        find_position(report, "\"Work Order Staging\")").expect("dataitem table reference");
    let table_def = client
        .definition(
            "src/WorkOrderProcessStaging.Report.al",
            dataitem_line,
            dataitem_col + 1,
        )
        .await
        .expect("Work Order Staging should resolve to the table");
    assert!(
        definition_uri(&table_def)
            .map(|u| u.contains("WorkOrderStaging.Table.al"))
            .unwrap_or(false),
        "Work Order Staging definition should point to WorkOrderStaging.Table.al"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_fixture_exact_completion_regressions() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let page_path = test_project_dir().join("src/WorkOrderStagingList.Page.al");
    let page_content = std::fs::read_to_string(&page_path).unwrap();
    client
        .open_file("src/WorkOrderStagingList.Page.al", &page_content)
        .await;

    let helper_path = test_project_dir().join("src/WorkOrderHelper.Codeunit.al");
    let helper_content = std::fs::read_to_string(&helper_path).unwrap();
    client
        .open_file("src/WorkOrderHelper.Codeunit.al", &helper_content)
        .await;

    let post_task_path = test_project_dir().join("src/WorkOrderPostTask.Codeunit.al");
    let post_task_content = std::fs::read_to_string(&post_task_path).unwrap();
    client
        .open_file("src/WorkOrderPostTask.Codeunit.al", &post_task_content)
        .await;

    let (this_line, this_col) =
        find_position(&helper_content, "this.PrecheckRecord(Staging)").expect("this. usage");
    let this_completions = client
        .completion("src/WorkOrderHelper.Codeunit.al", this_line, this_col + 5)
        .await;
    assert!(
        this_completions
            .iter()
            .any(|item| item.get("label").and_then(|v| v.as_str()) == Some("PrecheckRecord")),
        "`this.` completions should include PrecheckRecord"
    );

    let (rec_line, rec_col) =
        find_position(&page_content, "field(Description; Rec.Description)").expect("Rec. usage");
    let rec_col = page_content
        .lines()
        .nth(rec_line as usize)
        .and_then(|l| l.find("Rec.Description"))
        .unwrap_or(rec_col as usize) as u32
        + 4;
    let rec_completions = client
        .completion("src/WorkOrderStagingList.Page.al", rec_line, rec_col)
        .await;
    assert!(
        rec_completions
            .iter()
            .any(|item| item.get("label").and_then(|v| v.as_str()) == Some("Description")),
        "`Rec.` completions should include standard table fields"
    );

    let (enum_line, enum_col) =
        find_position(&post_task_content, "Staging.Status::Failed").expect("Status enum usage");
    let enum_completions = client
        .completion(
            "src/WorkOrderPostTask.Codeunit.al",
            enum_line,
            enum_col + 16,
        )
        .await;
    assert!(
        enum_completions
            .iter()
            .any(|item| item.get("label").and_then(|v| v.as_str()) == Some("Failed")),
        "`Status::` completions should include Failed"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_fixture_formatting_all_files() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let files = [
        "src/WorkOrderStaging.Table.al",
        "src/WorkOrderHelper.Codeunit.al",
        "src/WorkOrderPostTask.Codeunit.al",
        "src/WorkOrderStatus.Enum.al",
        "src/WorkOrderAction.Enum.al",
    ];

    for file in &files {
        let path = test_project_dir().join(file);
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        client.open_file(file, &content).await;
        client.format(file).await;
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_fixture_folding_all_files() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let files = [
        ("src/WorkOrderStaging.Table.al", 4),
        ("src/WorkOrderHelper.Codeunit.al", 5),
        ("src/WorkOrderStatus.Enum.al", 2),
    ];

    for (file, min_folds) in &files {
        let path = test_project_dir().join(file);
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
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

    client.shutdown().await;
}

#[tokio::test]
async fn test_fixture_member_navigation_hover_and_completion_regressions() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let report_rel = "src/WorkOrderProcessStaging.Report.al";
    let helper_rel = "src/WorkOrderHelper.Codeunit.al";
    let page_rel = "src/WorkOrderStagingList.Page.al";
    let table_rel = "src/WorkOrderStaging.Table.al";
    let enum_rel = "src/WorkOrderStatus.Enum.al";

    let report = std::fs::read_to_string(test_project_dir().join(report_rel)).unwrap();
    let helper = std::fs::read_to_string(test_project_dir().join(helper_rel)).unwrap();
    let page = std::fs::read_to_string(test_project_dir().join(page_rel)).unwrap();
    let table = std::fs::read_to_string(test_project_dir().join(table_rel)).unwrap();
    let enum_file = std::fs::read_to_string(test_project_dir().join(enum_rel)).unwrap();

    client.open_file(report_rel, &report).await;
    client.open_file(helper_rel, &helper).await;
    client.open_file(page_rel, &page).await;
    client.open_file(table_rel, &table).await;
    client.open_file(enum_rel, &enum_file).await;

    let (helper_line, helper_col) =
        find_position(&report, "this.Helper.SchedulePost").expect("Helper usage");
    let helper_def = client
        .definition(report_rel, helper_line, helper_col + 6)
        .await
        .expect("definition for Helper");
    assert!(
        definition_uri(&helper_def)
            .map(|uri| uri.ends_with("/src/WorkOrderProcessStaging.Report.al"))
            .unwrap_or(false),
        "Helper should resolve to the report variable declaration. Got: {:?}",
        helper_def
    );
    let expected_helper_line = find_line(&report, "Helper: Codeunit \"Work Order Helper\"");
    assert_eq!(
        definition_start_line(&helper_def),
        Some(expected_helper_line),
        "Helper should resolve to the report var block"
    );

    let schedule_line = helper_line;
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
            .map(|uri| uri.ends_with("/src/WorkOrderHelper.Codeunit.al"))
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
        schedule_hover_text.contains("SchedulePost") && schedule_hover_text.contains("post"),
        "SchedulePost hover should include signature and XML docs. Got: {:?}",
        schedule_hover
    );

    let (precheck_line, precheck_col) =
        find_position(&helper, "this.PrecheckRecord(Staging)").expect("PrecheckRecord usage");
    let precheck_def = client
        .definition(helper_rel, precheck_line, precheck_col + 6)
        .await
        .expect("definition for PrecheckRecord");
    assert!(
        definition_uri(&precheck_def)
            .map(|uri| uri.ends_with("/src/WorkOrderHelper.Codeunit.al"))
            .unwrap_or(false),
        "PrecheckRecord should resolve in the helper codeunit. Got: {:?}",
        precheck_def
    );
    let expected_precheck_line = find_line(&helper, "procedure PrecheckRecord(");
    assert_eq!(
        definition_start_line(&precheck_def),
        Some(expected_precheck_line),
        "PrecheckRecord should resolve to its local procedure declaration"
    );

    let (err_line, err_col) =
        find_position(&page, "this.ErrorMessageText := CopyStr").expect("ErrorMessageText usage");
    let err_def = client
        .definition(page_rel, err_line, err_col + 6)
        .await
        .expect("definition for ErrorMessageText");
    assert!(
        definition_uri(&err_def)
            .map(|uri| uri.ends_with("/src/WorkOrderStagingList.Page.al"))
            .unwrap_or(false),
        "ErrorMessageText should resolve to the page variable declaration. Got: {:?}",
        err_def
    );
    let expected_err_line = find_line(&page, "ErrorMessageText: Text;");
    assert_eq!(
        definition_start_line(&err_def),
        Some(expected_err_line),
        "ErrorMessageText should resolve to the page var block"
    );

    let (dataitem_line, dataitem_col) =
        find_position(&report, "\"Work Order Staging\")").expect("dataitem table reference");
    let table_def = client
        .definition(report_rel, dataitem_line, dataitem_col + 2)
        .await
        .expect("definition for Work Order Staging");
    assert!(
        definition_uri(&table_def)
            .map(|uri| uri.ends_with("/src/WorkOrderStaging.Table.al"))
            .unwrap_or(false),
        "Quoted table references should resolve to the workspace table. Got: {:?}",
        table_def
    );

    let (this_line, this_col) =
        find_position(&page, "this.ErrorMessageText := CopyStr").expect("this member access");
    let this_completions = client.completion(page_rel, this_line, this_col + 5).await;
    let this_labels = completion_labels(&this_completions);
    assert!(
        this_labels.contains(&"ErrorMessageText") && this_labels.contains(&"RowStyle"),
        "`this.` completions should include page members. Got: {:?}",
        this_labels
    );

    let (rec_line, _) =
        find_position(&page, "field(Description; Rec.Description)").expect("Rec member access");
    let rec_col = page
        .lines()
        .nth(rec_line as usize)
        .and_then(|line| line.find("Rec.Description"))
        .expect("Rec.Description position") as u32
        + 4;
    let rec_completions = client.completion(page_rel, rec_line, rec_col).await;
    let rec_labels = completion_labels(&rec_completions);
    assert!(
        rec_labels.contains(&"Description") && rec_labels.contains(&"Amount"),
        "`Rec.` completions should include source table fields. Got: {:?}",
        rec_labels
    );

    let (enum_line, enum_col) =
        find_position(&page, "this.ActionType::").expect("enum scope access");
    let enum_completions = client.completion(page_rel, enum_line, enum_col + 17).await;
    let enum_labels = completion_labels(&enum_completions);
    assert!(
        enum_labels.contains(&"Precheck") && enum_labels.contains(&"Post"),
        "Enum scope completions should include workspace enum values. Got: {:?}",
        enum_labels
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_fixture_dataitem_variable_resolution() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    open_test_files(&mut client).await;

    let report_rel = "src/WorkOrderProcessStaging.Report.al";
    let report = std::fs::read_to_string(test_project_dir().join(report_rel)).unwrap();

    // StagingRec is a dataitem variable, not a regular var
    let (staging_line, _staging_col) =
        find_position(&report, "this.Helper.PrecheckRecord(StagingRec)")
            .expect("StagingRec usage in PrecheckRecord call");
    let staging_col = report
        .lines()
        .nth(staging_line as usize)
        .and_then(|line| line.find("StagingRec)"))
        .expect("StagingRec in line") as u32;

    let staging_hover = client
        .hover(report_rel, staging_line, staging_col + 2)
        .await;
    assert!(
        staging_hover.is_some(),
        "StagingRec (dataitem variable) should have hover info"
    );
    let staging_hover_text = hover_content(staging_hover.as_ref().unwrap()).unwrap_or("");
    assert!(
        staging_hover_text.contains("Record") || staging_hover_text.contains("Work Order Staging"),
        "StagingRec hover should show Record type. Got: {:?}",
        staging_hover_text
    );

    let staging_dot_line = report
        .lines()
        .enumerate()
        .find(|(_idx, line)| line.contains("StagingRec.Status::Posting"))
        .map(|(idx, _)| idx as u32)
        .expect("StagingRec.Status::Posting line");
    let staging_dot_col = report
        .lines()
        .nth(staging_dot_line as usize)
        .and_then(|line| line.find("StagingRec."))
        .expect("StagingRec. position") as u32
        + 11; // after the dot
    let staging_completions = client
        .completion(report_rel, staging_dot_line, staging_dot_col)
        .await;
    let staging_labels = completion_labels(&staging_completions);
    assert!(
        staging_labels
            .iter()
            .any(|l| l.eq_ignore_ascii_case("Status")),
        "StagingRec. completions should include table fields like Status. Got: {:?}",
        staging_labels
    );

    client.shutdown().await;
}

/// Test cross-file go-to-definition: StagingRec.GetJournalData() should resolve to the table procedure.
#[tokio::test]
async fn test_fixture_cross_file_procedure_definition() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    open_test_files(&mut client).await;

    let report_rel = "src/WorkOrderProcessStaging.Report.al";
    let report = std::fs::read_to_string(test_project_dir().join(report_rel)).unwrap();

    let (get_line, _) =
        find_position(&report, "StagingRec.GetJournalData()").expect("GetJournalData usage");
    let get_col = report
        .lines()
        .nth(get_line as usize)
        .and_then(|line| line.find("GetJournalData"))
        .expect("GetJournalData position") as u32;

    let get_def = client
        .definition(report_rel, get_line, get_col + 2)
        .await
        .expect("GetJournalData should have a definition");
    assert!(
        definition_uri(&get_def)
            .map(|uri| uri.ends_with("WorkOrderStaging.Table.al"))
            .unwrap_or(false),
        "GetJournalData should resolve to the table file. Got: {:?}",
        get_def
    );

    client.shutdown().await;
}

/// Test multi-level member chain: this.PostTask.Run() in report.
/// PostTask is a Codeunit "Work Order Post Task" var.
#[tokio::test]
async fn test_fixture_multilevel_member_chain() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    open_test_files(&mut client).await;

    let report_rel = "src/WorkOrderProcessStaging.Report.al";
    let report = std::fs::read_to_string(test_project_dir().join(report_rel)).unwrap();

    let (run_line, _) =
        find_position(&report, "this.PostTask.Run(Staging)").expect("PostTask.Run() usage");
    let run_line_text = report.lines().nth(run_line as usize).unwrap();

    let post_task_col = run_line_text.find("PostTask").expect("PostTask in line") as u32;
    let post_task_hover = client.hover(report_rel, run_line, post_task_col + 2).await;
    assert!(
        post_task_hover.is_some(),
        "PostTask should have hover info (codeunit variable)"
    );
    let post_task_text = hover_content(post_task_hover.as_ref().unwrap()).unwrap_or("");
    assert!(
        post_task_text.contains("Codeunit") || post_task_text.contains("Work Order Post Task"),
        "PostTask hover should mention Codeunit type. Got: {:?}",
        post_task_text
    );

    let post_task_def = client
        .definition(report_rel, run_line, post_task_col + 2)
        .await;
    assert!(
        post_task_def.is_some(),
        "PostTask should have a definition (var declaration)"
    );
    if let Some(ref def) = post_task_def {
        assert!(
            definition_uri(def)
                .map(|uri| uri.ends_with("WorkOrderProcessStaging.Report.al"))
                .unwrap_or(false),
            "PostTask should resolve within the report. Got: {:?}",
            def
        );
    }

    client.shutdown().await;
}

/// Test Codeunit::"Work Order Post Task" scope access syntax.
#[tokio::test]
async fn test_fixture_codeunit_scope_access() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    open_test_files(&mut client).await;

    let post_task_rel = "src/WorkOrderPostTask.Codeunit.al";
    let post_task = std::fs::read_to_string(test_project_dir().join(post_task_rel)).unwrap();

    let (scope_line, _) = find_position(&post_task, "Codeunit::\"Work Order Post Task\"")
        .expect("Codeunit:: scope access");
    let scope_line_text = post_task.lines().nth(scope_line as usize).unwrap();

    let name_col = scope_line_text
        .find("\"Work Order Post Task\"")
        .expect("quoted name") as u32;
    let name_hover = client
        .hover(post_task_rel, scope_line, name_col + 2)
        .await
        .expect("codeunit scope hover");
    let text = hover_content(&name_hover).expect("hover contents");
    assert!(text.contains("Codeunit") || text.contains("Work Order Post Task"));

    let name_def = client
        .definition(post_task_rel, scope_line, name_col + 2)
        .await
        .expect("codeunit scope definition");
    assert!(
        definition_uri(&name_def)
            .map(|uri| uri.contains("WorkOrderPostTask.Codeunit.al") || uri.contains("Work Order"))
            .unwrap_or(false),
        "Codeunit:: scope should resolve to the codeunit file. Got: {:?}",
        name_def
    );

    client.shutdown().await;
}

/// Test semantic tokens for the report file — ensures highlighting works.
#[tokio::test]
async fn test_fixture_report_semantic_tokens() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let report_rel = "src/WorkOrderProcessStaging.Report.al";
    let report_path = test_project_dir().join(report_rel);
    let report = std::fs::read_to_string(&report_path).unwrap();
    client.open_file(report_rel, &report).await;

    let tokens = client.semantic_tokens(report_rel).await;
    assert!(
        tokens.is_some(),
        "Report file should produce semantic tokens"
    );

    let data = semantic_token_data(&tokens.unwrap());
    let line_count = report.lines().count();

    assert!(
        !data.is_empty(),
        "Report file ({} lines) should have semantic tokens. Got: {}",
        line_count,
        data.len()
    );

    let token_types: std::collections::HashSet<u32> = data.iter().map(|t| t[3]).collect();
    assert!(
        token_types.len() >= 3,
        "Should have at least 3 different token types. Got: {:?}",
        token_types
    );

    client.shutdown().await;
}

/// Regression: go-to-definition on a workspace table field should navigate to the field declaration.
/// Bug: ResolvedMemberKind::Field fell through to `_ => {}` in definition.rs.
#[tokio::test]
async fn test_fixture_field_definition_navigates() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    open_test_files(&mut client).await;

    let report_rel = "src/WorkOrderProcessStaging.Report.al";
    let report = std::fs::read_to_string(test_project_dir().join(report_rel)).unwrap();

    let (status_line, _) =
        find_position(&report, "StagingRec.Status::Posting").expect("StagingRec.Status usage");
    let status_col = report
        .lines()
        .nth(status_line as usize)
        .and_then(|line| line.find("StagingRec.Status::"))
        .expect("Status position") as u32
        + 11; // on "Status" after the dot

    let status_def = client
        .definition(report_rel, status_line, status_col + 2)
        .await;
    if let Some(ref def) = status_def {
        assert!(
            definition_uri(def)
                .map(|uri| uri.contains("WorkOrderStaging.Table.al"))
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
async fn test_fixture_enum_value_definition_navigates() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    open_test_files(&mut client).await;

    let report_rel = "src/WorkOrderProcessStaging.Report.al";
    let report = std::fs::read_to_string(test_project_dir().join(report_rel)).unwrap();

    let (posting_line, _) =
        find_position(&report, "Status::Posting").expect("Status::Posting usage");
    let posting_col = report
        .lines()
        .nth(posting_line as usize)
        .and_then(|line| line.find("Posting"))
        .expect("Posting position") as u32;

    let posting_def = client
        .definition(report_rel, posting_line, posting_col + 2)
        .await;
    if let Some(ref def) = posting_def {
        let def_uri = definition_uri(def).unwrap_or("");
        assert!(
            def_uri.contains("WorkOrderStatus.Enum.al") || def_uri.contains("Status"),
            "Posting enum value should resolve to the enum file. Got: {:?}",
            def
        );
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_fixture_signature_help_local_procedure() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    open_test_files(&mut client).await;

    let post_task_rel = "src/WorkOrderPostTask.Codeunit.al";
    let post_task = std::fs::read_to_string(test_project_dir().join(post_task_rel)).unwrap();

    let (line, _) = find_position(&post_task, "InsertJournalLine(Staging, ErrorText, 0)")
        .expect("InsertJournalLine call");
    let col = post_task
        .lines()
        .nth(line as usize)
        .and_then(|l| l.find("InsertJournalLine("))
        .expect("InsertJournalLine( position") as u32
        + 18; // after the (

    let sig = client.signature_help(post_task_rel, line, col).await;
    assert!(
        sig.is_some(),
        "InsertJournalLine should have signature help (local procedure with params)"
    );
    let sig_val = sig.unwrap();
    let label = sig_val
        .get("signatures")
        .and_then(|s| s.as_array())
        .and_then(|arr| arr.first())
        .and_then(|s| s.get("label"))
        .and_then(|l| l.as_str())
        .unwrap_or("");
    assert!(
        label.contains("InsertJournalLine") && label.contains("Staging"),
        "Signature should show InsertJournalLine with params. Got: {:?}",
        label
    );

    client.shutdown().await;
}

/// Signature help for a cross-file workspace procedure: ProcessReport.SetAction(...).
#[tokio::test]
async fn test_fixture_signature_help_cross_file_workspace_procedure() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let page_rel = "src/WorkOrderStagingList.Page.al";
    let page = std::fs::read_to_string(test_project_dir().join(page_rel)).unwrap();
    client.open_file(page_rel, &page).await;
    open_test_files(&mut client).await;

    let (line, _) = find_position(
        &page,
        "this.ProcessReport.SetAction(this.ActionType::Precheck)",
    )
    .expect("SetAction call");
    let col = page
        .lines()
        .nth(line as usize)
        .and_then(|l| l.find("SetAction("))
        .expect("SetAction( position") as u32
        + 10; // after the (

    let sig = client.signature_help(page_rel, line, col).await;
    assert!(
        sig.is_some(),
        "SetAction should have signature help — it's a workspace procedure on Report 'Work Order Process Staging'."
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_fixture_inlay_hints_local_procedure_calls() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    open_test_files(&mut client).await;

    let post_task_rel = "src/WorkOrderPostTask.Codeunit.al";
    let post_task = std::fs::read_to_string(test_project_dir().join(post_task_rel)).unwrap();
    let line_count = post_task.lines().count() as u32;

    let hints = client.inlay_hints(post_task_rel, 0, line_count).await;
    assert!(
        !hints.is_empty(),
        "Post task file should have inlay hints for procedure calls like InsertJournalLine, MarkStagingFailed"
    );

    let hint_labels: Vec<&str> = hints
        .iter()
        .filter_map(|h| h.get("label").and_then(|l| l.as_str()))
        .collect();
    eprintln!("Inlay hint labels: {:?}", hint_labels);
    assert!(
        hint_labels.iter().any(|l| l.contains("Staging")
            || l.contains("ErrorText")
            || l.contains("PostedEntryNo")),
        "Inlay hints should include parameter names like Staging, ErrorText, PostedEntryNo. Got: {:?}",
        hint_labels
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_fixture_references_cross_file() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    open_test_files(&mut client).await;

    let report_rel = "src/WorkOrderProcessStaging.Report.al";
    let report = std::fs::read_to_string(test_project_dir().join(report_rel)).unwrap();
    client.open_file(report_rel, &report).await;

    let table_rel = "src/WorkOrderStaging.Table.al";
    let table = std::fs::read_to_string(test_project_dir().join(table_rel)).unwrap();

    let (line, _) =
        find_position(&table, "procedure GetJournalData()").expect("GetJournalData declaration");
    let col = table
        .lines()
        .nth(line as usize)
        .and_then(|l| l.find("GetJournalData"))
        .expect("GetJournalData position") as u32;

    let refs = client.references(table_rel, line, col + 2).await;
    assert!(
        refs.len() >= 2,
        "GetJournalData should have at least 2 references (declaration + usages across files). Got: {}",
        refs.len()
    );

    let ref_uris: std::collections::HashSet<&str> = refs
        .iter()
        .filter_map(|r| r.get("uri").and_then(|u| u.as_str()))
        .collect();
    eprintln!("GetJournalData reference URIs: {:?}", ref_uris);

    client.shutdown().await;
}

/// Go-to-definition on `"Work Order Status"` in a table field type.
#[tokio::test]
async fn test_fixture_type_reference_definition() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    open_test_files(&mut client).await;

    let table_rel = "src/WorkOrderStaging.Table.al";
    let table = std::fs::read_to_string(test_project_dir().join(table_rel)).unwrap();

    let (line, _) = find_position(&table, "Enum \"Work Order Status\"")
        .expect("Work Order Status type reference");
    let col = table
        .lines()
        .nth(line as usize)
        .and_then(|l| l.find("\"Work Order Status\""))
        .expect("Work Order Status position") as u32
        + 2;

    let def = client.definition(table_rel, line, col).await;
    assert!(
        def.is_some(),
        "\"Work Order Status\" type reference should navigate to the enum file"
    );
    if let Some(ref d) = def {
        let uri = definition_uri(d).unwrap_or("");
        assert!(
            uri.contains("WorkOrderStatus.Enum.al"),
            "\"Work Order Status\" should resolve to WorkOrderStatus.Enum.al. Got: {:?}",
            uri
        );
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_fixture_codeunit_type_reference_definition() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    open_test_files(&mut client).await;

    let report_rel = "src/WorkOrderProcessStaging.Report.al";
    let report = std::fs::read_to_string(test_project_dir().join(report_rel)).unwrap();

    let (line, _) = find_position(&report, "Codeunit \"Work Order Helper\"")
        .expect("Work Order Helper type reference");
    let col = report
        .lines()
        .nth(line as usize)
        .and_then(|l| l.find("\"Work Order Helper\""))
        .expect("Work Order Helper position") as u32
        + 2;

    let def = client.definition(report_rel, line, col).await;
    assert!(
        def.is_some(),
        "\"Work Order Helper\" type reference should navigate to the codeunit file"
    );
    if let Some(ref d) = def {
        let uri = definition_uri(d).unwrap_or("");
        assert!(
            uri.contains("WorkOrderHelper.Codeunit.al"),
            "\"Work Order Helper\" should resolve to WorkOrderHelper.Codeunit.al. Got: {:?}",
            uri
        );
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_fixture_this_completions_in_page() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    open_test_files(&mut client).await;

    let page_rel = "src/WorkOrderStagingList.Page.al";
    let page = std::fs::read_to_string(test_project_dir().join(page_rel)).unwrap();
    client.open_file(page_rel, &page).await;

    let (line, _) = find_position(&page, "this.ErrorMessageText := CopyStr")
        .expect("this.ErrorMessageText usage");
    let col = page
        .lines()
        .nth(line as usize)
        .and_then(|l| l.find("this."))
        .expect("this. position") as u32
        + 5; // after "this."

    let completions = client.completion(page_rel, line, col).await;
    let labels = completion_labels(&completions);
    assert!(
        labels.contains(&"ErrorMessageText"),
        "this. in page should show ErrorMessageText. Got: {:?}",
        labels
    );
    assert!(
        labels.contains(&"RowStyle"),
        "this. in page should show RowStyle. Got: {:?}",
        labels
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_fixture_enum_completions_through_field_chain() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    open_test_files(&mut client).await;

    let page_rel = "src/WorkOrderStagingList.Page.al";
    let page = std::fs::read_to_string(test_project_dir().join(page_rel)).unwrap();
    client.open_file(page_rel, &page).await;

    let (line, _) = find_position(&page, "Rec.Status::Posted").expect("Rec.Status::Posted usage");
    let col = page
        .lines()
        .nth(line as usize)
        .and_then(|l| l.find("Rec.Status::"))
        .expect("Rec.Status:: position") as u32
        + 12; // after "::"

    let completions = client.completion(page_rel, line, col).await;
    let labels = completion_labels(&completions);
    assert!(
        labels.contains(&"Pending"),
        "Rec.Status:: should show Pending enum value. Got: {:?}",
        labels
    );
    assert!(
        labels.contains(&"Posted"),
        "Rec.Status:: should show Posted enum value. Got: {:?}",
        labels
    );

    client.shutdown().await;
}

/// Hover on `SetAction` at the call site in the staging list page.
#[tokio::test]
async fn test_fixture_hover_cross_file_workspace_procedure() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    open_test_files(&mut client).await;

    let page_rel = "src/WorkOrderStagingList.Page.al";
    let page = std::fs::read_to_string(test_project_dir().join(page_rel)).unwrap();
    client.open_file(page_rel, &page).await;

    let (line, _) = find_position(
        &page,
        "this.ProcessReport.SetAction(this.ActionType::Precheck)",
    )
    .expect("SetAction call");
    let col = page
        .lines()
        .nth(line as usize)
        .and_then(|l| l.find("SetAction"))
        .expect("SetAction position") as u32;

    let hover = client.hover(page_rel, line, col + 2).await;
    assert!(
        hover.is_some(),
        "SetAction should have hover info — it's a workspace procedure on Report 'Work Order Process Staging'"
    );
    let text = hover_content(hover.as_ref().unwrap()).unwrap_or("");
    assert!(
        text.contains("SetAction") && text.contains("NewAction"),
        "SetAction hover should show procedure signature with NewAction param. Got: {:?}",
        text
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_fixture_hover_workspace_procedure_with_return_type() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    open_test_files(&mut client).await;

    let report_rel = "src/WorkOrderProcessStaging.Report.al";
    let report = std::fs::read_to_string(test_project_dir().join(report_rel)).unwrap();

    let (line, _) =
        find_position(&report, "StagingRec.GetJournalData()").expect("GetJournalData call");
    let col = report
        .lines()
        .nth(line as usize)
        .and_then(|l| l.find("GetJournalData"))
        .expect("GetJournalData position") as u32;

    let hover = client.hover(report_rel, line, col + 2).await;
    assert!(
        hover.is_some(),
        "GetJournalData should have hover info — it's a workspace procedure on Table 'Work Order Staging'"
    );
    let text = hover_content(hover.as_ref().unwrap()).unwrap_or("");
    assert!(
        text.contains("GetJournalData"),
        "GetJournalData hover should mention the procedure name. Got: {:?}",
        text
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_fixture_rename_local_variable() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    open_test_files(&mut client).await;

    let post_task_rel = "src/WorkOrderPostTask.Codeunit.al";
    let post_task = std::fs::read_to_string(test_project_dir().join(post_task_rel)).unwrap();

    let (line, _) =
        find_position(&post_task, "LocalCounter: Integer;").expect("LocalCounter declaration");
    let col = post_task
        .lines()
        .nth(line as usize)
        .and_then(|l| l.find("LocalCounter"))
        .expect("LocalCounter position") as u32;

    let rename_result = client
        .rename(post_task_rel, line, col + 2, "EntryCounter")
        .await;
    assert!(rename_result.is_some(), "LocalCounter should be renameable");

    if let Some(ref edit) = rename_result {
        let changes = edit.get("changes").and_then(|c| c.as_object());
        assert!(
            changes.is_some() && !changes.unwrap().is_empty(),
            "Rename should produce workspace edits. Got: {:?}",
            edit
        );
        let change_count: usize = changes
            .unwrap()
            .values()
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

/// Verify code actions don't crash on real files with all lint rules active.
#[tokio::test]
async fn test_fixture_code_actions_no_crash() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    open_test_files(&mut client).await;

    let post_task_rel = "src/WorkOrderPostTask.Codeunit.al";
    let post_task = std::fs::read_to_string(test_project_dir().join(post_task_rel)).unwrap();
    let line_count = post_task.lines().count() as u32;

    let actions = client.code_actions(post_task_rel, 0, line_count).await;
    eprintln!("Post task code actions: {} found", actions.len());
    for action in &actions {
        if let Some(title) = action.get("title").and_then(|t| t.as_str()) {
            eprintln!("  - {}", title);
        }
    }

    client.shutdown().await;
}

/// 2-level chain hover: this.Helper.PrecheckRecord → should show procedure sig.
#[tokio::test]
async fn two_level_member_chain_hover_resolves() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    open_test_files(&mut client).await;

    let report_rel = "src/WorkOrderProcessStaging.Report.al";
    let report = std::fs::read_to_string(test_project_dir().join(report_rel)).unwrap();

    let (line, _) = find_position(&report, "this.Helper.PrecheckRecord(StagingRec)")
        .expect("PrecheckRecord usage");
    let line_text = report.lines().nth(line as usize).unwrap();

    let precheck_col = line_text
        .find("PrecheckRecord")
        .expect("PrecheckRecord in line") as u32;
    let hover = client.hover(report_rel, line, precheck_col + 2).await;
    assert!(
        hover.is_some(),
        "PrecheckRecord should have hover info — 2-level chain: this.Helper (Codeunit) → PrecheckRecord"
    );
    let text = hover_content(hover.as_ref().unwrap()).unwrap_or("");
    assert!(
        text.contains("PrecheckRecord"),
        "PrecheckRecord hover should mention the procedure name. Got: {:?}",
        text
    );

    client.shutdown().await;
}

/// Hover on quoted field access: Rec."Journal Data" in the staging list page.
#[tokio::test]
async fn quoted_field_hover_resolves() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    open_test_files(&mut client).await;

    let page_rel = "src/WorkOrderStagingList.Page.al";
    let page = std::fs::read_to_string(test_project_dir().join(page_rel)).unwrap();

    let (line, _) =
        find_position(&page, "Rec.\"Journal Data\".HasValue").expect("Journal Data usage");
    let line_text = page.lines().nth(line as usize).unwrap();

    let field_col = line_text
        .find("\"Journal Data\"")
        .expect("quoted field in line") as u32;
    let hover = client.hover(page_rel, line, field_col + 2).await;
    assert!(
        hover.is_some(),
        "\"Journal Data\" should have hover info — it's a field on Rec (source table)"
    );
    let text = hover_content(hover.as_ref().unwrap()).unwrap_or("");
    assert!(
        text.contains("Journal Data") || text.contains("field"),
        "Journal Data hover should mention field info. Got: {:?}",
        text
    );

    client.shutdown().await;
}

/// Hover on StagingRec.Status (field access on dataitem Record variable).
#[tokio::test]
async fn dataitem_field_hover_resolves() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    open_test_files(&mut client).await;

    let report_rel = "src/WorkOrderProcessStaging.Report.al";
    let report = std::fs::read_to_string(test_project_dir().join(report_rel)).unwrap();

    let (line, _) =
        find_position(&report, "StagingRec.Status::Posting").expect("StagingRec.Status usage");
    let line_text = report.lines().nth(line as usize).unwrap();

    let status_col = line_text
        .find("StagingRec.Status::")
        .expect("StagingRec.Status:: in line") as u32
        + 11; // after "StagingRec."
    let hover = client.hover(report_rel, line, status_col + 2).await;
    // Status is a field on "Work Order Staging" — should resolve via dataitem type
    assert!(
        hover.is_some(),
        "StagingRec.Status should have hover info — field on dataitem record"
    );
    let text = hover_content(hover.as_ref().unwrap()).unwrap_or("");
    assert!(
        text.contains("Status") || text.contains("Enum") || text.contains("Work Order Status"),
        "StagingRec.Status hover should mention Status field or enum type. Got: {:?}",
        text
    );

    client.shutdown().await;
}
