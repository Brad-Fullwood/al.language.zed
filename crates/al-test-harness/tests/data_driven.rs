//! Data-driven tests — hundreds of specific input→output assertions against the
//! real AL test project.  Each test starts ONE server, opens ALL files, then runs
//! a batch of checks so the total wall-clock time stays manageable.

use al_test_harness::*;
use std::path::PathBuf;

fn test_project_dir() -> PathBuf {
    test_project_from_env().expect("AL_TEST_PROJECT_PATH must be set to run fixture tests")
}

fn symbol_names_recursive(symbols: &[serde_json::Value]) -> Vec<String> {
    let mut names = vec![];
    for sym in symbols {
        if let Some(name) = sym.get("name").and_then(|n| n.as_str()) {
            names.push(name.to_string());
        }
        if let Some(children) = sym.get("children").and_then(|c| c.as_array()) {
            names.extend(symbol_names_recursive(children));
        }
    }
    names
}

async fn open_all_test_files(client: &mut LspClient) {
    let files = [
        "objects/API/ItemJournalStaging.Table.al",
        "objects/API/ItemJournalAPI.Page.al",
        "objects/Automation/IJLAPIHelper.Codeunit.al",
        "objects/Automation/IJLPostTask.Codeunit.al",
        "objects/Automation/IJLStatus.Enum.al",
        "objects/Testing/IJLProcessStaging.Report.al",
        "objects/Testing/IJLProcessAction.Enum.al",
        "objects/Testing/IJLStagingList.Page.al",
        "objects/Testing/IJLJournalEntryCard.Page.al",
        "objects/System/JsonTools.Codeunit.al",
        "objects/System/IJLInstall.Codeunit.al",
        "objects/System/IJLAPI.PermissionSet.al",
    ];
    for file in &files {
        let path = test_project_dir().join(file);
        if path.exists() {
            let content = std::fs::read_to_string(&path).unwrap();
            client.open_file(file, &content).await;
        } else {
            // Don't silently skip — surface missing fixtures so a stale
            // AL_TEST_PROJECT_PATH or a renamed object is loud, not invisible.
            // (eprintln! shows up in `cargo test -- --nocapture` and is
            // captured by CI logs.)
            eprintln!(
                "data_driven: missing test fixture {} (resolved {})",
                file,
                path.display()
            );
        }
    }
}

/// (file, line, col, substring_that_must_appear_in_hover)
const HOVER_CASES: &[(&str, u32, u32, &str)] = &[
    // ── Table: ItemJournalStaging.Table.al ──
    // Procedure declarations (0-based line numbers; col 30 = inside procedure name)
    (
        "objects/API/ItemJournalStaging.Table.al",
        120,
        30,
        "procedure SetJournalData",
    ),
    (
        "objects/API/ItemJournalStaging.Table.al",
        129,
        30,
        "procedure GetJournalData",
    ),
    (
        "objects/API/ItemJournalStaging.Table.al",
        142,
        30,
        "procedure SetErrorMessage",
    ),
    (
        "objects/API/ItemJournalStaging.Table.al",
        151,
        30,
        "procedure GetErrorMessage",
    ),
    // Local variable types (0-based line numbers; col 10 = inside variable name)
    (
        "objects/API/ItemJournalStaging.Table.al",
        122,
        10,
        "OutStream",
    ), // OutStream var
    (
        "objects/API/ItemJournalStaging.Table.al",
        131,
        10,
        "InStream",
    ), // InStream var
    ("objects/API/ItemJournalStaging.Table.al", 132, 10, "Text"), // Data: Text
    // ── Codeunit: IJLAPIHelper.Codeunit.al ──
    // Procedure declarations
    (
        "objects/Automation/IJLAPIHelper.Codeunit.al",
        9,
        20,
        "procedure Precheck",
    ),
    (
        "objects/Automation/IJLAPIHelper.Codeunit.al",
        49,
        20,
        "procedure SchedulePost",
    ),
    (
        "objects/Automation/IJLAPIHelper.Codeunit.al",
        66,
        20,
        "procedure GetItemJournalFromStaging",
    ),
    (
        "objects/Automation/IJLAPIHelper.Codeunit.al",
        93,
        20,
        "procedure ApplyStagingFilter",
    ),
    // Local variables in PrecheckRecord
    (
        "objects/Automation/IJLAPIHelper.Codeunit.al",
        21,
        10,
        "Record",
    ), // TempItemJnlLine: Record "Item Journal Line"
    // Local variables in GetItemJournalFromStaging
    (
        "objects/Automation/IJLAPIHelper.Codeunit.al",
        68,
        10,
        "Codeunit",
    ), // JsonTools: Codeunit "Json Tools"
    (
        "objects/Automation/IJLAPIHelper.Codeunit.al",
        69,
        10,
        "JsonObject",
    ), // JsonObj: JsonObject
    (
        "objects/Automation/IJLAPIHelper.Codeunit.al",
        70,
        10,
        "Text",
    ), // JournalData: Text
    // Local variables in ApplyStagingFilter
    (
        "objects/Automation/IJLAPIHelper.Codeunit.al",
        95,
        10,
        "RecordRef",
    ), // RecRef: RecordRef
    (
        "objects/Automation/IJLAPIHelper.Codeunit.al",
        97,
        10,
        "FieldRef",
    ), // FldRef: FieldRef
    (
        "objects/Automation/IJLAPIHelper.Codeunit.al",
        99,
        10,
        "Dictionary",
    ), // FieldMap
    (
        "objects/Automation/IJLAPIHelper.Codeunit.al",
        100,
        8,
        "Integer",
    ), // i: Integer
    // ── Codeunit: IJLPostTask.Codeunit.al ──
    // Global label variables
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        11,
        10,
        "Label",
    ), // JournalTemplateTok
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        12,
        10,
        "Label",
    ), // JournalBatchTok
    // Procedure declarations
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        22,
        25,
        "procedure ProcessPostingQueue",
    ),
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        50,
        25,
        "procedure EnsureJournalBatchExists",
    ),
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        68,
        25,
        "procedure ClearJournalBatch",
    ),
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        78,
        25,
        "procedure InsertJournalLine",
    ),
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        95,
        25,
        "procedure ModifyWithDataFromStandardCode",
    ),
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        106,
        25,
        "procedure NormaliseQuantity",
    ),
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        116,
        25,
        "procedure PopulateFromProductionOrder",
    ),
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        138,
        25,
        "procedure ProcessDimensions",
    ),
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        172,
        25,
        "procedure PostBatch",
    ),
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        193,
        25,
        "procedure MarkAllPosted",
    ),
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        206,
        25,
        "procedure PostLinesSingly",
    ),
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        231,
        25,
        "procedure MarkStagingPosted",
    ),
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        239,
        25,
        "procedure MarkStagingFailed",
    ),
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        247,
        25,
        "procedure GetLastPostedEntryNo",
    ),
    // Local variables in ProcessPostingQueue
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        24,
        12,
        "Record",
    ), // StagingRec
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        25,
        12,
        "Record",
    ), // ItemJnlLine
    // ── Page: ItemJournalAPI.Page.al ──
    // Global variable types
    ("objects/API/ItemJournalAPI.Page.al", 168, 12, "Record"), // StagingRec: Record "Item Journal Staging"
    ("objects/API/ItemJournalAPI.Page.al", 169, 12, "Text"),   // StatusText: Text
    ("objects/API/ItemJournalAPI.Page.al", 170, 12, "Text"),   // ErrorMessageText: Text
    // Procedure declarations (line 232 = [ServiceEnabled], line 233 = procedure Precheck; 0-based lines)
    (
        "objects/API/ItemJournalAPI.Page.al",
        232,
        15,
        "procedure Precheck",
    ),
    (
        "objects/API/ItemJournalAPI.Page.al",
        250,
        15,
        "procedure Post",
    ),
    // Local variables in Precheck
    ("objects/API/ItemJournalAPI.Page.al", 234, 12, "Record"), // FilteredStaging
    ("objects/API/ItemJournalAPI.Page.al", 235, 12, "Codeunit"), // APIHelper
    // ── Report: IJLProcessStaging.Report.al ──
    // Global variable types
    (
        "objects/Testing/IJLProcessStaging.Report.al",
        72,
        12,
        "Codeunit",
    ), // APIHelper
    (
        "objects/Testing/IJLProcessStaging.Report.al",
        73,
        12,
        "Codeunit",
    ), // IJLPostTask
    (
        "objects/Testing/IJLProcessStaging.Report.al",
        74,
        12,
        "Enum",
    ), // ActionType
    (
        "objects/Testing/IJLProcessStaging.Report.al",
        75,
        12,
        "Boolean",
    ), // RunSynchronously
    // Procedure declarations
    (
        "objects/Testing/IJLProcessStaging.Report.al",
        77,
        30,
        "procedure SetAction",
    ),
    (
        "objects/Testing/IJLProcessStaging.Report.al",
        82,
        30,
        "procedure SetRunSynchronously",
    ),
    // ── Page: IJLStagingList.Page.al ──
    // Global variable types
    ("objects/Testing/IJLStagingList.Page.al", 143, 12, "Enum"), // ActionType
    ("objects/Testing/IJLStagingList.Page.al", 144, 12, "Text"), // ErrorMessageText
    ("objects/Testing/IJLStagingList.Page.al", 145, 12, "Text"), // RowStyle
    // Action trigger local vars (text-based fallback)
    ("objects/Testing/IJLStagingList.Page.al", 72, 25, "Report"), // ProcessReport in RunPrecheck
    // ── Page: IJLJournalEntryCard.Page.al ──
    // Global flags
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        285,
        12,
        "Boolean",
    ), // IsAssemblyEntry
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        286,
        12,
        "Boolean",
    ), // IsConsumptionEntry
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        287,
        12,
        "Boolean",
    ), // IsOrderBasedEntry
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        288,
        12,
        "Boolean",
    ), // IsOutputEntry
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        289,
        12,
        "Boolean",
    ), // IsProductionEntry
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        290,
        12,
        "Boolean",
    ), // IsTransferEntry
    // Procedure declarations
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        292,
        25,
        "procedure UpdateEntryTypeFlags",
    ),
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        302,
        25,
        "procedure LookupOrderNo",
    ),
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        313,
        25,
        "procedure LookupProductionOrder",
    ),
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        328,
        25,
        "procedure LookupAssemblyOrder",
    ),
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        343,
        25,
        "procedure LookupOrderLineNo",
    ),
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        360,
        25,
        "procedure LookupProdOrderLineNo",
    ),
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        370,
        25,
        "procedure LookupProdOrderCompLineNo",
    ),
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        380,
        25,
        "procedure LookupAssemblyLineNo",
    ),
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        390,
        25,
        "procedure LookupItemNo",
    ),
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        397,
        25,
        "procedure LookupItemNoForOrder",
    ),
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        414,
        25,
        "procedure LookupItemFromProdOrderLine",
    ),
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        424,
        25,
        "procedure LookupItemFromProdOrderComp",
    ),
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        434,
        25,
        "procedure LookupItemFromAssemblyHeader",
    ),
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        444,
        25,
        "procedure LookupItemFromAssemblyLine",
    ),
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        454,
        25,
        "procedure SelectProdOrderLine",
    ),
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        468,
        25,
        "procedure SelectProdOrderComponent",
    ),
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        482,
        25,
        "procedure SelectAssemblyHeader",
    ),
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        496,
        25,
        "procedure SelectAssemblyLine",
    ),
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        511,
        25,
        "procedure SelectItem",
    ),
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        524,
        25,
        "procedure CreateStagingRecord",
    ),
    // Local variables in CreateStagingRecord
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        526,
        12,
        "Record",
    ), // StagingRec
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        527,
        12,
        "Codeunit",
    ), // JsonTools
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        528,
        12,
        "JsonObject",
    ), // JsonObj
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        529,
        12,
        "Text",
    ), // JsonText
    // ── Codeunit: JsonTools.Codeunit.al ──
    (
        "objects/System/JsonTools.Codeunit.al",
        2,
        28,
        "procedure Json2Rec",
    ),
    (
        "objects/System/JsonTools.Codeunit.al",
        46,
        28,
        "procedure Rec2Json",
    ),
    // Local vars in Rec2Json
    ("objects/System/JsonTools.Codeunit.al", 48, 12, "RecordRef"), // RecRef
    ("objects/System/JsonTools.Codeunit.al", 49, 12, "FieldRef"),  // FieldRef var
    ("objects/System/JsonTools.Codeunit.al", 50, 12, "JsonObject"), // JsonOutput
    ("objects/System/JsonTools.Codeunit.al", 52, 12, "Integer"),   // i
    // ── Codeunit: IJLInstall.Codeunit.al ──
    (
        "objects/System/IJLInstall.Codeunit.al",
        10,
        20,
        "procedure AddRetentionPolicyAllowedTables",
    ),
    ("objects/System/IJLInstall.Codeunit.al", 12, 12, "Codeunit"), // RetenPolAllowedTables
    ("objects/System/IJLInstall.Codeunit.al", 13, 12, "Integer"),  // MinRetentionDays
    // ── `this` implicit variable ──
    // `this` in codeunit → should resolve to Codeunit
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        19,
        10,
        "Codeunit",
    ), // this.ProcessPostingQueue
    // `this` in page → should resolve to Page
    ("objects/Testing/IJLStagingList.Page.al", 153, 14, "Text"), // this.ErrorMessageText
    // `this` in report → should resolve to Report
    (
        "objects/Testing/IJLProcessStaging.Report.al",
        19,
        22,
        "Report",
    ), // this.ActionType
    // ── Package symbol hovers (objects from .app files) ──
    // "Item Journal Staging" should be a known workspace Table
    (
        "objects/API/ItemJournalStaging.Table.al",
        0,
        20,
        "Item Journal Staging",
    ),
];

#[ignore = "requires AL_TEST_PROJECT_PATH environment variable"]
#[tokio::test]
async fn test_hover_data_driven() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    open_all_test_files(&mut client).await;

    let mut passed = 0u32;
    let mut failed = 0u32;
    let total = HOVER_CASES.len();

    for (i, &(file, line, col, expected_substr)) in HOVER_CASES.iter().enumerate() {
        let result = client.hover(file, line, col).await;
        let md = result.as_ref().and_then(|r| hover_content(r));

        match md {
            Some(text) if text.contains(expected_substr) => {
                passed += 1;
            }
            Some(text) => {
                eprintln!(
                    "HOVER FAIL [{}/{}] {}:{}:{} — expected '{}' in hover, got:\n  {}",
                    i + 1,
                    total,
                    file,
                    line + 1,
                    col + 1,
                    expected_substr,
                    text.lines().next().unwrap_or("")
                );
                failed += 1;
            }
            None => {
                eprintln!(
                    "HOVER FAIL [{}/{}] {}:{}:{} — expected '{}' but hover returned null",
                    i + 1,
                    total,
                    file,
                    line + 1,
                    col + 1,
                    expected_substr
                );
                failed += 1;
            }
        }
    }

    client.shutdown().await;

    eprintln!("\nHover results: {passed}/{total} passed, {failed} failed");
    assert_eq!(failed, 0, "{failed} hover assertions failed out of {total}");
}

/// (file, line, col, expected_target_file_contains, expected_line_near)
/// expected_line_near: Some(n) means the definition should be near line n (±5)
/// None means we only check the file.
const DEFINITION_CASES: &[(&str, u32, u32, &str, Option<u32>)] = &[
    // ── Cross-file: workspace object names ──
    // "Item Journal Staging" in SourceTable → should go to table file
    (
        "objects/Testing/IJLStagingList.Page.al",
        7,
        30,
        "ItemJournalStaging.Table.al",
        Some(0),
    ),
    // "IJL API Helper" codeunit reference → should go to codeunit file
    (
        "objects/Testing/IJLProcessStaging.Report.al",
        72,
        30,
        "IJLAPIHelper.Codeunit.al",
        Some(0),
    ),
    // "IJL Post Task" codeunit reference → should go to codeunit file
    (
        "objects/Testing/IJLProcessStaging.Report.al",
        73,
        30,
        "IJLPostTask.Codeunit.al",
        Some(0),
    ),
    // "IJL Process Action" enum reference → should go to enum file
    (
        "objects/Testing/IJLProcessStaging.Report.al",
        74,
        25,
        "IJLProcessAction.Enum.al",
        Some(0),
    ),
    // "IJL Status" enum reference in table field → should go to enum file
    (
        "objects/API/ItemJournalStaging.Table.al",
        28,
        35,
        "IJLStatus.Enum.al",
        Some(0),
    ),
    // "Json Tools" codeunit reference → should go to codeunit file
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        527,
        30,
        "JsonTools.Codeunit.al",
        Some(0),
    ),
    // "IJL Staging List" page reference in table
    (
        "objects/API/ItemJournalStaging.Table.al",
        5,
        30,
        "IJLStagingList.Page.al",
        Some(0),
    ),
    // ── Same-file: procedure definitions ──
    // ProcessPostingQueue call → should go to definition
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        19,
        15,
        "IJLPostTask.Codeunit.al",
        Some(22),
    ),
    // EnsureJournalBatchExists call → definition
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        32,
        15,
        "IJLPostTask.Codeunit.al",
        Some(50),
    ),
    // ClearJournalBatch call → definition
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        33,
        15,
        "IJLPostTask.Codeunit.al",
        Some(68),
    ),
    // InsertJournalLine call → definition
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        36,
        25,
        "IJLPostTask.Codeunit.al",
        Some(78),
    ),
    // MarkStagingFailed call → definition
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        37,
        21,
        "IJLPostTask.Codeunit.al",
        Some(239),
    ),
    // NormaliseQuantity call → definition
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        99,
        15,
        "IJLPostTask.Codeunit.al",
        Some(106),
    ),
    // PopulateFromProductionOrder call → definition
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        100,
        15,
        "IJLPostTask.Codeunit.al",
        Some(116),
    ),
    // PostBatch call → definition
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        47,
        15,
        "IJLPostTask.Codeunit.al",
        Some(172),
    ),
    // MarkAllPosted call → definition
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        185,
        17,
        "IJLPostTask.Codeunit.al",
        Some(193),
    ),
    // PostLinesSingly call → definition
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        190,
        15,
        "IJLPostTask.Codeunit.al",
        Some(206),
    ),
    // GetLastPostedEntryNo call → definition
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        202,
        52,
        "IJLPostTask.Codeunit.al",
        Some(247),
    ),
    // LookupOrderNo in EntryCard → definition
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        57,
        36,
        "IJLJournalEntryCard.Page.al",
        Some(302),
    ),
    // LookupOrderLineNo in EntryCard → definition
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        67,
        36,
        "IJLJournalEntryCard.Page.al",
        Some(343),
    ),
    // LookupItemNo in EntryCard → definition
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        113,
        36,
        "IJLJournalEntryCard.Page.al",
        Some(390),
    ),
    // UpdateEntryTypeFlags in EntryCard → definition (col 29 = 'U', not 28 which is '.')
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        38,
        29,
        "IJLJournalEntryCard.Page.al",
        Some(292),
    ),
    // CreateStagingRecord in EntryCard → definition
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        280,
        18,
        "IJLJournalEntryCard.Page.al",
        Some(524),
    ),
    // ── Same-file: local variable references → first occurrence ──
    // StagingRec in ProcessPostingQueue → declaration at line 24
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        28,
        10,
        "IJLPostTask.Codeunit.al",
        Some(24),
    ),
    // ── Cross-file: procedure calls ──
    // PrecheckRecord call from Precheck body → definition
    (
        "objects/Automation/IJLAPIHelper.Codeunit.al",
        15,
        18,
        "IJLAPIHelper.Codeunit.al",
        Some(19),
    ),
    // TryCheckItemJnlLine call → definition
    (
        "objects/Automation/IJLAPIHelper.Codeunit.al",
        34,
        18,
        "IJLAPIHelper.Codeunit.al",
        Some(81),
    ),
    // GetItemJournalFromStaging call → definition
    (
        "objects/Automation/IJLAPIHelper.Codeunit.al",
        26,
        25,
        "IJLAPIHelper.Codeunit.al",
        Some(66),
    ),
    // BuildFilterFieldMap call → definition
    (
        "objects/Automation/IJLAPIHelper.Codeunit.al",
        104,
        15,
        "IJLAPIHelper.Codeunit.al",
        Some(124),
    ),
    // GetSupportedFilterNames call → definition
    (
        "objects/Automation/IJLAPIHelper.Codeunit.al",
        115,
        55,
        "IJLAPIHelper.Codeunit.al",
        Some(136),
    ),
    // ── Variable type definitions ──
    // StagingRec declaration type "Item Journal Staging" → table file
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        24,
        40,
        "ItemJournalStaging.Table.al",
        Some(0),
    ),
];

#[ignore = "requires AL_TEST_PROJECT_PATH environment variable"]
#[tokio::test]
async fn test_definition_data_driven() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    open_all_test_files(&mut client).await;

    let mut passed = 0u32;
    let mut failed = 0u32;
    let total = DEFINITION_CASES.len();

    for (i, &(file, line, col, expected_file, expected_line)) in DEFINITION_CASES.iter().enumerate()
    {
        let result = client.definition(file, line, col).await;

        let ok = match &result {
            Some(r) => {
                let uri = definition_uri(r);
                let def_line = definition_start_line(r);
                let file_ok = uri.is_some_and(|u| u.contains(expected_file));
                let line_ok = match (expected_line, def_line) {
                    (Some(exp), Some(got)) => (got as i64 - exp as i64).unsigned_abs() <= 5,
                    (None, _) => true,
                    (Some(_), None) => false,
                };
                file_ok && line_ok
            }
            None => false,
        };

        if ok {
            passed += 1;
        } else {
            let uri = result
                .as_ref()
                .and_then(|r| definition_uri(r))
                .unwrap_or("null");
            let line_got = result.as_ref().and_then(definition_start_line);
            eprintln!(
                "DEF FAIL [{}/{}] {}:{}:{} — expected file='{}' line={:?}, got uri='{}' line={:?}",
                i + 1,
                total,
                file,
                line + 1,
                col + 1,
                expected_file,
                expected_line,
                uri,
                line_got
            );
            failed += 1;
        }
    }

    client.shutdown().await;

    eprintln!("\nDefinition results: {passed}/{total} passed, {failed} failed");
    assert_eq!(
        failed, 0,
        "{failed} definition assertions failed out of {total}"
    );
}

/// (file, line, col, items_that_must_be_present, items_that_must_not_be_present)
type CompletionCase = (
    &'static str,
    u32,
    u32,
    &'static [&'static str],
    &'static [&'static str],
);
const COMPLETION_CASES: &[CompletionCase] = &[
    // ── this. completions in report ──
    // this. in report body → should show global vars + procedures
    (
        "objects/Testing/IJLProcessStaging.Report.al",
        18,
        17,
        &[
            "APIHelper",
            "IJLPostTask",
            "ActionType",
            "RunSynchronously",
            "SetAction",
            "SetRunSynchronously",
        ],
        &[],
    ),
    // ── this. completions in page (IJL Staging List) ──
    // this. in trigger body → should show global vars
    (
        "objects/Testing/IJLStagingList.Page.al",
        153,
        13,
        &["ActionType", "ErrorMessageText", "RowStyle"],
        &[],
    ),
    // ── this. completions in codeunit (IJLPostTask) ──
    // this. in trigger OnRun → should show procedures and labels
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        19,
        13,
        &[
            "ProcessPostingQueue",
            "JournalTemplateTok",
            "JournalBatchTok",
        ],
        &[],
    ),
    // ── this. completions in codeunit (IJLAPIHelper) ──
    (
        "objects/Automation/IJLAPIHelper.Codeunit.al",
        15,
        17,
        &[
            "PrecheckRecord",
            "GetItemJournalFromStaging",
            "TryCheckItemJnlLine",
            "BuildFilterFieldMap",
        ],
        &[],
    ),
    // ── this. completions in page (IJLJournalEntryCard) ──
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        38,
        24,
        &[
            "UpdateEntryTypeFlags",
            "IsOutputEntry",
            "IsConsumptionEntry",
            "IsProductionEntry",
            "IsAssemblyEntry",
            "IsOrderBasedEntry",
            "IsTransferEntry",
            "LookupOrderNo",
            "LookupItemNo",
            "CreateStagingRecord",
        ],
        &[],
    ),
    // ── Rec. completions in page (IJL Staging List, SourceTable = Item Journal Staging) ──
    // Rec. in field reference → should show staging table fields
    (
        "objects/Testing/IJLStagingList.Page.al",
        19,
        39,
        &[
            "Entry No.",
            "Status",
            "Entry Type",
            "Item No.",
            "Document No.",
            "Posting Date",
            "Location Code",
            "Order No.",
            "Posted Entry No.",
            "Processed At",
        ],
        &[],
    ),
    // ── Default (no receiver) completions in procedure body ──
    // Inside a procedure, typing at the start → should include local vars
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        28,
        8,
        &["StagingRec", "ItemJnlLine"],
        &[],
    ),
    // ── this. completions in JsonTools codeunit ──
    (
        "objects/System/JsonTools.Codeunit.al",
        7,
        14,
        &["Json2Rec", "GetJsonFieldName"],
        &[],
    ),
    // ── this. completions in IJLInstall codeunit ──
    (
        "objects/System/IJLInstall.Codeunit.al",
        7,
        13,
        &["AddRetentionPolicyAllowedTables"],
        &[],
    ),
];

#[ignore = "requires AL_TEST_PROJECT_PATH environment variable"]
#[tokio::test]
async fn test_completions_data_driven() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    open_all_test_files(&mut client).await;

    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut total_checks = 0u32;

    for (i, &(file, line, col, must_have, must_not_have)) in COMPLETION_CASES.iter().enumerate() {
        let items = client.completion(file, line, col).await;
        let labels = completion_labels(&items);

        for expected in must_have {
            total_checks += 1;
            if labels.iter().any(|l| l.eq_ignore_ascii_case(expected)) {
                passed += 1;
            } else {
                eprintln!(
                    "COMPLETION FAIL [{}] {}:{}:{} — '{}' not found in {} completions. Got: {:?}",
                    i + 1,
                    file,
                    line + 1,
                    col + 1,
                    expected,
                    labels.len(),
                    &labels[..labels.len().min(20)]
                );
                failed += 1;
            }
        }

        for forbidden in must_not_have {
            total_checks += 1;
            if labels.iter().any(|l| l.eq_ignore_ascii_case(forbidden)) {
                eprintln!(
                    "COMPLETION FAIL [{}] {}:{}:{} — '{}' should NOT appear in completions",
                    i + 1,
                    file,
                    line + 1,
                    col + 1,
                    forbidden
                );
                failed += 1;
            } else {
                passed += 1;
            }
        }
    }

    client.shutdown().await;

    eprintln!("\nCompletion results: {passed}/{total_checks} passed, {failed} failed");
    assert_eq!(
        failed, 0,
        "{failed} completion assertions failed out of {total_checks}"
    );
}

/// (file, symbols_that_must_be_present)
const SYMBOL_CASES: &[(&str, &[&str])] = &[
    // Table
    (
        "objects/API/ItemJournalStaging.Table.al",
        &[
            "Item Journal Staging",
            "SetJournalData",
            "GetJournalData",
            "SetErrorMessage",
            "GetErrorMessage",
        ],
    ),
    // Page: ItemJournalAPI
    (
        "objects/API/ItemJournalAPI.Page.al",
        &[
            "Item Journal API",
            "Precheck",
            "Post",
            "OnAfterGetRecord",
            "OnInsertRecord",
            "OnFindRecord",
        ],
    ),
    // Codeunit: IJLAPIHelper
    (
        "objects/Automation/IJLAPIHelper.Codeunit.al",
        &[
            "IJL API Helper",
            "Precheck",
            "PrecheckRecord",
            "SchedulePost",
            "GetItemJournalFromStaging",
            "TryCheckItemJnlLine",
            "ApplyStagingFilter",
            "BuildFilterFieldMap",
            "GetSupportedFilterNames",
        ],
    ),
    // Codeunit: IJLPostTask
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        &[
            "IJL Post Task",
            "OnRun",
            "ProcessPostingQueue",
            "EnsureJournalBatchExists",
            "ClearJournalBatch",
            "InsertJournalLine",
            "ModifyWithDataFromStandardCode",
            "NormaliseQuantity",
            "PopulateFromProductionOrder",
            "ProcessDimensions",
            "PostBatch",
            "MarkAllPosted",
            "PostLinesSingly",
            "MarkStagingPosted",
            "MarkStagingFailed",
            "GetLastPostedEntryNo",
        ],
    ),
    // Enum: IJLStatus
    ("objects/Automation/IJLStatus.Enum.al", &["IJL Status"]),
    // Report: IJLProcessStaging
    (
        "objects/Testing/IJLProcessStaging.Report.al",
        &[
            "IJL Process Staging",
            "SetAction",
            "SetRunSynchronously",
            "OnPreDataItem",
            "OnPostReport",
        ],
    ),
    // Enum: IJLProcessAction
    (
        "objects/Testing/IJLProcessAction.Enum.al",
        &["IJL Process Action"],
    ),
    // Page: IJLStagingList
    (
        "objects/Testing/IJLStagingList.Page.al",
        &["IJL Staging List", "OnAfterGetRecord"],
    ),
    // Page: IJLJournalEntryCard
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        &[
            "IJL Journal Entry Card",
            "OnOpenPage",
            "OnQueryClosePage",
            "UpdateEntryTypeFlags",
            "LookupOrderNo",
            "LookupProductionOrder",
            "LookupAssemblyOrder",
            "LookupOrderLineNo",
            "LookupProdOrderLineNo",
            "LookupProdOrderCompLineNo",
            "LookupAssemblyLineNo",
            "LookupItemNo",
            "LookupItemNoForOrder",
            "LookupItemFromProdOrderLine",
            "LookupItemFromProdOrderComp",
            "LookupItemFromAssemblyHeader",
            "LookupItemFromAssemblyLine",
            "SelectProdOrderLine",
            "SelectProdOrderComponent",
            "SelectAssemblyHeader",
            "SelectAssemblyLine",
            "SelectItem",
            "CreateStagingRecord",
        ],
    ),
    // Codeunit: JsonTools
    (
        "objects/System/JsonTools.Codeunit.al",
        &["Json Tools", "Json2Rec", "Rec2Json"],
    ),
    // Codeunit: IJLInstall
    (
        "objects/System/IJLInstall.Codeunit.al",
        &[
            "IJL Install",
            "OnInstallAppPerCompany",
            "AddRetentionPolicyAllowedTables",
            "OnRefreshAllowedTables",
        ],
    ),
    // PermissionSet
    ("objects/System/IJLAPI.PermissionSet.al", &["IJL API"]),
];

#[ignore = "requires AL_TEST_PROJECT_PATH environment variable"]
#[tokio::test]
async fn test_document_symbols_data_driven() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    open_all_test_files(&mut client).await;

    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut total_checks = 0u32;

    for (i, &(file, expected_syms)) in SYMBOL_CASES.iter().enumerate() {
        let symbols = client.document_symbols(file).await;
        let names = symbol_names_recursive(&symbols);

        for expected in expected_syms {
            total_checks += 1;
            if names.iter().any(|n| n.eq_ignore_ascii_case(expected)) {
                passed += 1;
            } else {
                eprintln!(
                    "SYMBOL FAIL [{}] {} — '{}' not found. Got: {:?}",
                    i + 1,
                    file,
                    expected,
                    &names[..names.len().min(20)]
                );
                failed += 1;
            }
        }
    }

    client.shutdown().await;

    eprintln!("\nDocument symbol results: {passed}/{total_checks} passed, {failed} failed");
    assert_eq!(
        failed, 0,
        "{failed} symbol assertions failed out of {total_checks}"
    );
}

/// (file, line, col, expected_substr_in_signature)
const SIGNATURE_CASES: &[(&str, u32, u32, &str)] = &[
    // Local procedure call in IJLPostTask
    // this.InsertJournalLine(StagingRec, ItemJnlLine) — 2 params
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        36,
        42,
        "InsertJournalLine",
    ),
    // this.MarkStagingFailed(StagingRec, ...) — 2 params
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        37,
        39,
        "MarkStagingFailed",
    ),
    // this.ModifyWithDataFromStandardCode(ItemJnlLine) — 1 param
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        41,
        55,
        "ModifyWithDataFromStandardCode",
    ),
    // this.NormaliseQuantity(ItemJournalLine) — 1 param
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        99,
        40,
        "NormaliseQuantity",
    ),
    // this.PopulateFromProductionOrder(ItemJournalLine) — 1 param
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        100,
        50,
        "PopulateFromProductionOrder",
    ),
    // this.ProcessDimensions(ItemJournalLine) — 1 param
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        101,
        40,
        "ProcessDimensions",
    ),
    // this.MarkStagingPosted(StagingRec, ...) — 2 params
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        202,
        40,
        "MarkStagingPosted",
    ),
    // Procedure calls in IJLAPIHelper
    // this.PrecheckRecord(Staging) — 1 param
    (
        "objects/Automation/IJLAPIHelper.Codeunit.al",
        15,
        35,
        "PrecheckRecord",
    ),
    // this.GetItemJournalFromStaging(Staging, TempItemJnlLine) — 2 params
    (
        "objects/Automation/IJLAPIHelper.Codeunit.al",
        26,
        50,
        "GetItemJournalFromStaging",
    ),
    // this.TryCheckItemJnlLine(TempItemJnlLine) — 1 param
    (
        "objects/Automation/IJLAPIHelper.Codeunit.al",
        34,
        45,
        "TryCheckItemJnlLine",
    ),
    // this.BuildFilterFieldMap(SourceRec, FieldMap) — 2 params
    (
        "objects/Automation/IJLAPIHelper.Codeunit.al",
        104,
        38,
        "BuildFilterFieldMap",
    ),
    // Cross-file: ProcessReport.SetAction in StagingList page action trigger
    (
        "objects/Testing/IJLStagingList.Page.al",
        72,
        45,
        "SetAction",
    ),
    // Cross-file: ProcessReport.SetRunSynchronously
    (
        "objects/Testing/IJLStagingList.Page.al",
        73,
        54,
        "SetRunSynchronously",
    ),
    // Procedure calls in IJLJournalEntryCard
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        57,
        49,
        "LookupOrderNo",
    ),
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        67,
        53,
        "LookupOrderLineNo",
    ),
    (
        "objects/Testing/IJLJournalEntryCard.Page.al",
        113,
        48,
        "LookupItemNo",
    ),
];

#[ignore = "requires AL_TEST_PROJECT_PATH environment variable"]
#[tokio::test]
async fn test_signature_help_data_driven() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    open_all_test_files(&mut client).await;

    let mut passed = 0u32;
    let mut failed = 0u32;
    let total = SIGNATURE_CASES.len();

    for (i, &(file, line, col, expected_substr)) in SIGNATURE_CASES.iter().enumerate() {
        let result = client.signature_help(file, line, col).await;
        let label = result.as_ref().and_then(|r| sig_label(r));

        match label {
            Some(text)
                if text
                    .to_lowercase()
                    .contains(&expected_substr.to_lowercase()) =>
            {
                passed += 1;
            }
            Some(text) => {
                eprintln!(
                    "SIG FAIL [{}/{}] {}:{}:{} — expected '{}' in signature, got: {}",
                    i + 1,
                    total,
                    file,
                    line + 1,
                    col + 1,
                    expected_substr,
                    text
                );
                failed += 1;
            }
            None => {
                eprintln!(
                    "SIG FAIL [{}/{}] {}:{}:{} — expected '{}' but signature help returned null",
                    i + 1,
                    total,
                    file,
                    line + 1,
                    col + 1,
                    expected_substr
                );
                failed += 1;
            }
        }
    }

    client.shutdown().await;

    eprintln!("\nSignature help results: {passed}/{total} passed, {failed} failed");
    assert_eq!(
        failed, 0,
        "{failed} signature help assertions failed out of {total}"
    );
}

/// (file, min_ranges_expected)
const FOLDING_CASES: &[(&str, usize)] = &[
    ("objects/API/ItemJournalStaging.Table.al", 10), // fields, keys, procedures
    ("objects/API/ItemJournalAPI.Page.al", 20),      // many field groups, triggers, procedures
    ("objects/Automation/IJLAPIHelper.Codeunit.al", 8), // procedures
    ("objects/Automation/IJLPostTask.Codeunit.al", 15), // many procedures
    ("objects/Automation/IJLStatus.Enum.al", 3),     // enum values
    ("objects/Testing/IJLProcessStaging.Report.al", 5), // dataset, requestpage, triggers, vars
    ("objects/Testing/IJLProcessAction.Enum.al", 2), // enum values
    ("objects/Testing/IJLStagingList.Page.al", 10),  // layout, actions, trigger
    ("objects/Testing/IJLJournalEntryCard.Page.al", 30), // many groups, many procedures
    ("objects/System/JsonTools.Codeunit.al", 6),     // procedures
    ("objects/System/IJLInstall.Codeunit.al", 3),    // trigger, procedures
    ("objects/System/IJLAPI.PermissionSet.al", 1),   // just the body
];

#[ignore = "requires AL_TEST_PROJECT_PATH environment variable"]
#[tokio::test]
async fn test_folding_data_driven() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    open_all_test_files(&mut client).await;

    let mut passed = 0u32;
    let mut failed = 0u32;
    let total = FOLDING_CASES.len();

    for (i, &(file, min_ranges)) in FOLDING_CASES.iter().enumerate() {
        let ranges = client.folding_ranges(file).await;

        if ranges.len() >= min_ranges {
            passed += 1;
        } else {
            eprintln!(
                "FOLD FAIL [{}/{}] {} — expected >= {} ranges, got {}",
                i + 1,
                total,
                file,
                min_ranges,
                ranges.len()
            );
            failed += 1;
        }
    }

    client.shutdown().await;

    eprintln!("\nFolding results: {passed}/{total} passed, {failed} failed");
    assert_eq!(
        failed, 0,
        "{failed} folding assertions failed out of {total}"
    );
}

/// (file, min_token_count)
const TOKEN_CASES: &[(&str, usize)] = &[
    ("objects/API/ItemJournalStaging.Table.al", 50),
    ("objects/API/ItemJournalAPI.Page.al", 80),
    ("objects/Automation/IJLAPIHelper.Codeunit.al", 50),
    ("objects/Automation/IJLPostTask.Codeunit.al", 80),
    ("objects/Automation/IJLStatus.Enum.al", 5),
    ("objects/Testing/IJLProcessStaging.Report.al", 30),
    ("objects/Testing/IJLProcessAction.Enum.al", 3),
    ("objects/Testing/IJLStagingList.Page.al", 50),
    ("objects/Testing/IJLJournalEntryCard.Page.al", 100),
    ("objects/System/JsonTools.Codeunit.al", 50),
    ("objects/System/IJLInstall.Codeunit.al", 10),
    ("objects/System/IJLAPI.PermissionSet.al", 5),
];

#[ignore = "requires AL_TEST_PROJECT_PATH environment variable"]
#[tokio::test]
async fn test_semantic_tokens_data_driven() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    open_all_test_files(&mut client).await;

    let mut passed = 0u32;
    let mut failed = 0u32;
    let total = TOKEN_CASES.len();

    for (i, &(file, min_tokens)) in TOKEN_CASES.iter().enumerate() {
        let result = client.semantic_tokens(file).await;
        let count = result
            .as_ref()
            .and_then(|r| r.get("data"))
            .and_then(|d| d.as_array())
            .map(|a| a.len() / 5) // Each token is 5 integers
            .unwrap_or(0);

        if count >= min_tokens {
            passed += 1;
        } else {
            eprintln!(
                "TOKEN FAIL [{}/{}] {} — expected >= {} tokens, got {}",
                i + 1,
                total,
                file,
                min_tokens,
                count
            );
            failed += 1;
        }
    }

    client.shutdown().await;

    eprintln!("\nSemantic token results: {passed}/{total} passed, {failed} failed");
    assert_eq!(
        failed, 0,
        "{failed} semantic token assertions failed out of {total}"
    );
}

/// (file, line, col, identifier_name, min_references_expected)
const REFERENCE_CASES: &[(&str, u32, u32, &str, usize)] = &[
    // "Entry No." field in staging table — used in many places
    (
        "objects/API/ItemJournalStaging.Table.al",
        10,
        20,
        "Entry No.",
        2,
    ),
    // Status field — used extensively
    (
        "objects/API/ItemJournalStaging.Table.al",
        28,
        20,
        "Status",
        2,
    ),
    // StagingRec in ProcessPostingQueue — used multiple times
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        24,
        10,
        "StagingRec",
        3,
    ),
    // ItemJnlLine in ProcessPostingQueue — used multiple times
    (
        "objects/Automation/IJLPostTask.Codeunit.al",
        25,
        10,
        "ItemJnlLine",
        3,
    ),
    // Staging param in Precheck — used in body
    (
        "objects/Automation/IJLAPIHelper.Codeunit.al",
        9,
        45,
        "Staging",
        3,
    ),
];

#[ignore = "requires AL_TEST_PROJECT_PATH environment variable"]
#[tokio::test]
async fn test_references_data_driven() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    open_all_test_files(&mut client).await;

    let mut passed = 0u32;
    let mut failed = 0u32;
    let total = REFERENCE_CASES.len();

    for (i, &(file, line, col, name, min_refs)) in REFERENCE_CASES.iter().enumerate() {
        let refs = client.references(file, line, col).await;

        if refs.len() >= min_refs {
            passed += 1;
        } else {
            eprintln!(
                "REF FAIL [{}/{}] {}:{}:{} ({}) — expected >= {} refs, got {}",
                i + 1,
                total,
                file,
                line + 1,
                col + 1,
                name,
                min_refs,
                refs.len()
            );
            failed += 1;
        }
    }

    client.shutdown().await;

    eprintln!("\nReference results: {passed}/{total} passed, {failed} failed");
    assert_eq!(
        failed, 0,
        "{failed} reference assertions failed out of {total}"
    );
}

const ZERO_DIAG_FILES: &[&str] = &[
    "objects/API/ItemJournalStaging.Table.al",
    "objects/Automation/IJLStatus.Enum.al",
    "objects/Testing/IJLProcessAction.Enum.al",
    "objects/System/IJLAPI.PermissionSet.al",
];

#[ignore = "requires AL_TEST_PROJECT_PATH environment variable"]
#[tokio::test]
async fn test_diagnostics_data_driven() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    open_all_test_files(&mut client).await;

    let _ = client.drain_diagnostics();

    let mut passed = 0u32;
    let mut failed = 0u32;
    let total = ZERO_DIAG_FILES.len();

    for (i, &file) in ZERO_DIAG_FILES.iter().enumerate() {
        let diags = client.drain_diagnostics();
        let file_diags = diags
            .iter()
            .filter(|(k, _)| {
                k.contains(&file.replace("/", "%2F").replace(" ", "%20")) || k.contains(file)
            })
            .flat_map(|(_, v)| v.iter())
            .count();

        if file_diags == 0 {
            passed += 1;
        } else {
            eprintln!(
                "DIAG FAIL [{}/{}] {} — expected 0 diagnostics, got {}",
                i + 1,
                total,
                file,
                file_diags
            );
            failed += 1;
        }
    }

    client.shutdown().await;

    eprintln!("\nDiagnostics results: {passed}/{total} passed, {failed} failed");
    assert_eq!(
        failed, 0,
        "{failed} diagnostic assertions failed out of {total}"
    );
}
