//! Checked-in data-driven LSP contract.
//!
//! The original suite was permanently ignored and referenced a private
//! `ItemJournal*` project that was not in this repository. It therefore added
//! no release evidence. This replacement exercises the bundled AL project and
//! fails immediately if a fixture is renamed or a binary request is not
//! acknowledged.

use al_test_harness::{test_project_dir, LspClient};
use serde_json::Value;
use std::path::PathBuf;

const OBJECTS: &[(&str, &str, usize)] = &[
    ("src/CodeunitWithEvents.al", "Test Event Publisher", 3),
    ("src/DeepNesting.al", "Deep Nesting", 5),
    ("src/DeeplyNestedActions.al", "Sales Order Pageext", 8),
    ("src/Enum50100.al", "Test Status", 1),
    ("src/HelloWorld.al", "Hello World", 2),
    ("src/Interface50100.al", "ITest Processor", 1),
    ("src/MultiProcedure.al", "Multi Procedure", 8),
    ("src/Page50100.al", "Test Customer Card", 5),
    ("src/PageExtension50100.al", "Test Customer Card Ext", 3),
    ("src/PermissionSet50123.al", "Test App Full", 1),
    ("src/PureLogicTest.Codeunit.al", "Pure Logic Test", 4),
    ("src/Query50121.al", "Customer Balances", 2),
    ("src/Report50120.al", "Customer List", 4),
    ("src/Table50100.al", "Test Customer", 4),
    ("src/TableExtension50100.al", "Test Customer Ext", 2),
    ("src/WorkOrderAction.Enum.al", "Work Order Action", 1),
    ("src/WorkOrderHelper.Codeunit.al", "Work Order Helper", 6),
    (
        "src/WorkOrderPostTask.Codeunit.al",
        "Work Order Post Task",
        7,
    ),
    (
        "src/WorkOrderProcessStaging.Report.al",
        "Work Order Process Staging",
        6,
    ),
    ("src/WorkOrderStaging.Table.al", "Work Order Staging", 7),
    (
        "src/WorkOrderStagingList.Page.al",
        "Work Order Staging List",
        9,
    ),
    ("src/WorkOrderStatus.Enum.al", "Work Order Status", 1),
    ("src/XmlPort50122.al", "Customer Export", 2),
];

fn fixture(path: &str) -> String {
    let absolute = test_project_dir().join(path);
    std::fs::read_to_string(&absolute)
        .unwrap_or_else(|error| panic!("fixture {} must be readable: {error}", absolute.display()))
}

fn position(source: &str, needle: &str, within: &str) -> (u32, u32) {
    for (line_index, line) in source.lines().enumerate() {
        if let Some(needle_column) = line.find(needle) {
            let within_column = line[needle_column..]
                .find(within)
                .unwrap_or_else(|| panic!("{within:?} is not inside {needle:?}"));
            return (
                u32::try_from(line_index).expect("fixture line fits u32"),
                u32::try_from(needle_column + within_column).expect("fixture column fits u32"),
            );
        }
    }
    panic!("fixture does not contain {needle:?}");
}

fn symbol_names(symbols: &[Value], output: &mut Vec<String>) {
    for symbol in symbols {
        if let Some(name) = symbol.get("name").and_then(Value::as_str) {
            output.push(name.to_string());
        }
        if let Some(children) = symbol.get("children").and_then(Value::as_array) {
            symbol_names(children, output);
        }
    }
}

fn hover_text(value: &Value) -> String {
    let contents = &value["contents"];
    if let Some(text) = contents.as_str() {
        return text.to_string();
    }
    if let Some(text) = contents.get("value").and_then(Value::as_str) {
        return text.to_string();
    }
    if let Some(parts) = contents.as_array() {
        return parts
            .iter()
            .filter_map(|part| {
                part.as_str()
                    .or_else(|| part.get("value").and_then(Value::as_str))
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    panic!("invalid hover response: {value}");
}

fn definition_uri(value: &Value) -> &str {
    if let Some(uri) = value.get("uri").and_then(Value::as_str) {
        return uri;
    }
    if let Some(uri) = value
        .as_array()
        .and_then(|locations| locations.first())
        .and_then(|location| location.get("uri"))
        .and_then(Value::as_str)
    {
        return uri;
    }
    panic!("invalid definition response: {value}");
}

async fn open_all(client: &mut LspClient) {
    for (path, _, _) in OBJECTS {
        client.open_file(path, &fixture(path)).await;
    }
}

#[tokio::test]
async fn every_checked_in_object_has_symbols_tokens_folds_and_acknowledged_diagnostics() {
    let mut client = LspClient::spawn(test_project_dir())
        .await
        .expect("start al-lsp");
    open_all(&mut client).await;

    for (path, object_name, minimum_folds) in OBJECTS {
        let mut names = Vec::new();
        symbol_names(&client.document_symbols(path).await, &mut names);
        assert!(
            names.iter().any(|name| name == object_name),
            "{path} did not expose object {object_name:?}; got {names:?}"
        );

        let tokens = client
            .semantic_tokens(path)
            .await
            .unwrap_or_else(|| panic!("{path} returned null semantic tokens"));
        let token_data = tokens["data"]
            .as_array()
            .unwrap_or_else(|| panic!("{path} returned invalid semantic tokens: {tokens}"));
        assert!(
            !token_data.is_empty() && token_data.len() % 5 == 0,
            "{path} returned malformed/empty semantic tokens"
        );

        let folds = client.folding_ranges(path).await;
        assert!(
            folds.len() >= *minimum_folds,
            "{path} exposed {} folds; expected at least {minimum_folds}",
            folds.len()
        );
    }

    let diagnostics = client.drain_diagnostics();
    for (path, _, _) in OBJECTS {
        let uri = client.file_uri(path);
        let entries = diagnostics
            .get(&uri)
            .unwrap_or_else(|| panic!("{path} had no acknowledged diagnostics publication"));
        assert!(
            entries
                .iter()
                .all(|diagnostic| diagnostic["severity"].as_u64() != Some(1)),
            "{path} has error diagnostics: {entries:#?}"
        );
    }

    client.shutdown().await;
}

#[tokio::test]
async fn hover_definition_completion_signature_and_references_match_fixture_contract() {
    let mut client = LspClient::spawn(test_project_dir())
        .await
        .expect("start al-lsp");
    open_all(&mut client).await;

    let helper_path = "src/WorkOrderHelper.Codeunit.al";
    let helper = fixture(helper_path);
    let (line, column) = position(&helper, "procedure PrecheckRecord", "PrecheckRecord");
    let hover = client
        .hover(helper_path, line, column + 2)
        .await
        .expect("PrecheckRecord hover");
    assert!(
        hover_text(&hover).contains("PrecheckRecord"),
        "unexpected procedure hover: {hover:#}"
    );

    let report_path = "src/WorkOrderProcessStaging.Report.al";
    let report = fixture(report_path);
    for (needle, symbol, expected_file) in [
        (
            "this.Helper.SchedulePost(Staging);",
            "SchedulePost",
            "WorkOrderHelper.Codeunit.al",
        ),
        (
            "this.PostTask.Run(Staging);",
            "Run",
            "WorkOrderPostTask.Codeunit.al",
        ),
        (
            "dataitem(Staging; \"Work Order Staging\")",
            "Work Order Staging",
            "WorkOrderStaging.Table.al",
        ),
    ] {
        let (line, column) = position(&report, needle, symbol);
        let definition = client
            .definition(report_path, line, column + 1)
            .await
            .unwrap_or_else(|| panic!("no definition for {symbol}"));
        assert!(
            definition_uri(&definition).ends_with(expected_file),
            "{symbol} resolved to {definition:#}"
        );
    }

    let page_path = "src/WorkOrderStagingList.Page.al";
    let page = fixture(page_path);
    let (line, column) = position(&page, "this.ProcessReport.SetAction", "this.");
    let labels = client
        .completion(page_path, line, column + 5)
        .await
        .into_iter()
        .filter_map(|item| {
            item.get("label")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect::<Vec<_>>();
    for expected in [
        "ProcessReport",
        "ActionType",
        "ErrorMessageText",
        "RowStyle",
    ] {
        assert!(
            labels.iter().any(|label| label == expected),
            "this. completion omitted {expected}; got {labels:?}"
        );
    }

    let post_path = "src/WorkOrderPostTask.Codeunit.al";
    let post = fixture(post_path);
    let (line, column) = position(
        &post,
        "InsertJournalLine(Staging, ErrorText, 0)",
        "ErrorText",
    );
    let signature = client
        .signature_help(post_path, line, column)
        .await
        .expect("InsertJournalLine signature");
    let label = signature["signatures"][0]["label"]
        .as_str()
        .expect("signature label");
    assert!(
        label.contains("InsertJournalLine") && label.contains("PostedEntryNo"),
        "unexpected signature: {signature:#}"
    );

    let references = client
        .references(helper_path, line_for(&helper, "procedure SchedulePost"), 16)
        .await;
    assert!(
        references.len() >= 2,
        "SchedulePost should include its declaration and call sites: {references:#?}"
    );

    client.shutdown().await;
}

fn line_for(source: &str, needle: &str) -> u32 {
    source
        .lines()
        .position(|line| line.contains(needle))
        .map(|line| u32::try_from(line).expect("fixture line fits u32"))
        .unwrap_or_else(|| panic!("fixture does not contain {needle:?}"))
}

#[test]
fn fixture_matrix_is_complete_and_has_no_external_path_dependency() {
    let src = test_project_dir().join("src");
    let mut actual = std::fs::read_dir(&src)
        .unwrap_or_else(|error| panic!("read {}: {error}", src.display()))
        .map(|entry| entry.expect("fixture directory entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "al"))
        .collect::<Vec<PathBuf>>();
    actual.sort();

    let mut expected = OBJECTS
        .iter()
        .map(|(path, _, _)| test_project_dir().join(path))
        .collect::<Vec<_>>();
    expected.sort();

    assert_eq!(
        actual, expected,
        "OBJECTS must cover every checked-in AL fixture exactly"
    );
}
