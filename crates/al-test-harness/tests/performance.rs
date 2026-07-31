//! Binary LSP latency smoke test against the checked-in AL fixture.
//!
//! This is intentionally an always-on hang/regression guard, not a shared-host
//! microbenchmark or a cross-machine SLA. Criterion owns deterministic timing
//! reports for parser, symbol/index, completion, impact, event, and memory hot
//! paths. The previous file was permanently ignored and named files from an
//! unavailable private project, so it produced no CI or release evidence.

use al_test_harness::{test_project_dir, LspClient};
use std::time::{Duration, Instant};

const ITERATIONS: usize = 7;
const MAX_MEDIAN: Duration = Duration::from_millis(250);

fn fixture(path: &str) -> String {
    let absolute = test_project_dir().join(path);
    std::fs::read_to_string(&absolute)
        .unwrap_or_else(|error| panic!("fixture {} must be readable: {error}", absolute.display()))
}

fn position(source: &str, needle: &str, within: &str) -> (u32, u32) {
    for (line_index, line) in source.lines().enumerate() {
        if let Some(start) = line.find(needle) {
            let within = line[start..]
                .find(within)
                .unwrap_or_else(|| panic!("{within:?} is not inside {needle:?}"));
            return (
                u32::try_from(line_index).expect("fixture line fits u32"),
                u32::try_from(start + within).expect("fixture column fits u32"),
            );
        }
    }
    panic!("fixture does not contain {needle:?}");
}

fn median(samples: &mut [Duration]) -> Duration {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

macro_rules! measure {
    ($label:literal, $operation:block) => {{
        $operation
        let mut samples = Vec::with_capacity(ITERATIONS);
        for _ in 0..ITERATIONS {
            let started = Instant::now();
            $operation
            samples.push(started.elapsed());
        }
        let measured = median(&mut samples);
        eprintln!(
            "{} median={measured:?} samples={samples:?}",
            $label
        );
        assert!(
            measured < MAX_MEDIAN,
            "{} median {measured:?} exceeded the {MAX_MEDIAN:?} binary smoke budget",
            $label
        );
    }};
}

#[tokio::test]
async fn checked_in_fixture_lsp_queries_complete_within_binary_smoke_budget() {
    let mut client = LspClient::spawn(test_project_dir())
        .await
        .expect("start al-lsp");

    let table_path = "src/WorkOrderStaging.Table.al";
    let helper_path = "src/WorkOrderHelper.Codeunit.al";
    let page_path = "src/WorkOrderStagingList.Page.al";
    let table = fixture(table_path);
    let helper = fixture(helper_path);
    let page = fixture(page_path);
    client.open_file(table_path, &table).await;
    client.open_file(helper_path, &helper).await;
    client.open_file(page_path, &page).await;

    let (hover_line, hover_column) =
        position(&helper, "procedure PrecheckRecord", "PrecheckRecord");
    measure!("hover", {
        assert!(
            client
                .hover(helper_path, hover_line, hover_column + 2)
                .await
                .is_some(),
            "hover unexpectedly returned null"
        );
    });

    let (completion_line, completion_column) =
        position(&page, "this.ProcessReport.SetAction", "this.");
    measure!("completion", {
        assert!(
            !client
                .completion(page_path, completion_line, completion_column + 5)
                .await
                .is_empty(),
            "completion unexpectedly returned no items"
        );
    });

    let (definition_line, definition_column) = position(
        &helper,
        "Staging: Record \"Work Order Staging\"",
        "Work Order Staging",
    );
    measure!("definition", {
        assert!(
            client
                .definition(helper_path, definition_line, definition_column + 2)
                .await
                .is_some(),
            "definition unexpectedly returned null"
        );
    });

    measure!("document symbols", {
        assert!(
            !client.document_symbols(table_path).await.is_empty(),
            "document symbols unexpectedly returned no items"
        );
    });

    measure!("semantic tokens", {
        let result = client
            .semantic_tokens(page_path)
            .await
            .expect("semantic tokens unexpectedly returned null");
        assert!(
            result["data"]
                .as_array()
                .is_some_and(|data| !data.is_empty()),
            "semantic token data unexpectedly empty"
        );
    });

    client.shutdown().await;
}
