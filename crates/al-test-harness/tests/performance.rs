//! Performance tests — measure latency of key LSP queries against the real
//! AL test project to verify they meet latency budgets.
//!
//! Targets (T803):
//! - hover: <10ms
//! - completions: <20ms
//! - definition: <10ms
//! - document symbols: <5ms
//! - semantic tokens: <15ms
//!
//! These tests are `#[ignore]` by default. To run them you must:
//!   1. Point `AL_TEST_PROJECT_PATH` at a real AL project on disk
//!      (the inline fixture under `data/test_al_project/` does NOT
//!      contain the deep-codebase files referenced below).
//!   2. Pass `-- --ignored` to cargo:
//!      `AL_TEST_PROJECT_PATH=/path/to/project \
//!         cargo test -p al-test-harness --test performance -- --ignored`
//!
//! T071 doc-fix: previously the file header said `cargo test -p al-test-harness`
//! would run these — that was wrong; without the env var the fixture
//! resolution panics, and without `--ignored` the tests are skipped.

use al_test_harness::*;
use std::path::PathBuf;
use std::time::Instant;

fn test_project_dir() -> PathBuf {
    test_project_from_env().expect("AL_TEST_PROJECT_PATH must be set to run fixture tests")
}

async fn open_test_files(client: &mut LspClient) {
    let files = [
        "objects/API/ItemJournalStaging.Table.al",
        "objects/Codeunit/IJLEventSubscribers.Codeunit.al",
        "objects/Page/IJLItemJournalStagingLine.Page.al",
    ];
    let project = test_project_dir();
    for f in &files {
        let path = project.join(f);
        if path.exists() {
            let content = std::fs::read_to_string(&path).unwrap();
            client.open_file(f, &content).await;
        }
    }
}

/// Run 5 iterations and return the median duration in microseconds.
fn median(durations: &mut [u64]) -> u64 {
    durations.sort();
    durations[durations.len() / 2]
}

const ITERATIONS: usize = 5;

#[ignore = "requires AL_TEST_PROJECT_PATH environment variable"]
#[tokio::test]
async fn test_hover_latency() {
    let mut client = LspClient::spawn(&test_project_dir()).await.unwrap();
    open_test_files(&mut client).await;

    // Warm up
    let _ = client
        .hover("objects/API/ItemJournalStaging.Table.al", 10, 10)
        .await;

    let mut durations = Vec::with_capacity(ITERATIONS);
    for _ in 0..ITERATIONS {
        let start = Instant::now();
        let _ = client
            .hover("objects/API/ItemJournalStaging.Table.al", 15, 20)
            .await;
        durations.push(start.elapsed().as_micros() as u64);
    }

    let median_ms = median(&mut durations) as f64 / 1000.0;
    eprintln!("hover median: {median_ms:.2}ms ({durations:?})");
    assert!(
        median_ms < 10.0,
        "hover must be <10ms, got {median_ms:.2}ms"
    );

    client.shutdown().await;
}

#[ignore = "requires AL_TEST_PROJECT_PATH environment variable"]
#[tokio::test]
async fn test_completion_latency() {
    let mut client = LspClient::spawn(&test_project_dir()).await.unwrap();
    open_test_files(&mut client).await;

    // Warm up
    let _ = client
        .completion("objects/Codeunit/IJLEventSubscribers.Codeunit.al", 10, 10)
        .await;

    let mut durations = Vec::with_capacity(ITERATIONS);
    for _ in 0..ITERATIONS {
        let start = Instant::now();
        let _ = client
            .completion("objects/Codeunit/IJLEventSubscribers.Codeunit.al", 20, 15)
            .await;
        durations.push(start.elapsed().as_micros() as u64);
    }

    let median_ms = median(&mut durations) as f64 / 1000.0;
    eprintln!("completion median: {median_ms:.2}ms ({durations:?})");
    assert!(
        median_ms < 20.0,
        "completion must be <20ms, got {median_ms:.2}ms"
    );

    client.shutdown().await;
}

#[ignore = "requires AL_TEST_PROJECT_PATH environment variable"]
#[tokio::test]
async fn test_definition_latency() {
    let mut client = LspClient::spawn(&test_project_dir()).await.unwrap();
    open_test_files(&mut client).await;

    // Warm up — use a position that triggers package symbol lookup to warm caches
    let _ = client
        .definition("objects/API/ItemJournalStaging.Table.al", 10, 10)
        .await;

    // Test at procedure declaration name (line 120, col 30 = "SetJournalData")
    // This tests workspace-local definition resolution
    let mut durations = Vec::with_capacity(ITERATIONS);
    for _ in 0..ITERATIONS {
        let start = Instant::now();
        let _ = client
            .definition("objects/API/ItemJournalStaging.Table.al", 120, 30)
            .await;
        durations.push(start.elapsed().as_micros() as u64);
    }

    let median_ms = median(&mut durations) as f64 / 1000.0;
    eprintln!("definition median: {median_ms:.2}ms ({durations:?})");
    // Note: definition includes stdio IPC round-trip. Budget is handler-side <10ms,
    // but we allow 2x for IPC overhead in end-to-end measurement.
    assert!(
        median_ms < 20.0,
        "definition must be <20ms end-to-end, got {median_ms:.2}ms"
    );

    client.shutdown().await;
}

#[ignore = "requires AL_TEST_PROJECT_PATH environment variable"]
#[tokio::test]
async fn test_document_symbols_latency() {
    let mut client = LspClient::spawn(&test_project_dir()).await.unwrap();
    open_test_files(&mut client).await;

    // Warm up
    let _ = client
        .document_symbols("objects/API/ItemJournalStaging.Table.al")
        .await;

    let mut durations = Vec::with_capacity(ITERATIONS);
    for _ in 0..ITERATIONS {
        let start = Instant::now();
        let _ = client
            .document_symbols("objects/API/ItemJournalStaging.Table.al")
            .await;
        durations.push(start.elapsed().as_micros() as u64);
    }

    let median_ms = median(&mut durations) as f64 / 1000.0;
    eprintln!("document_symbols median: {median_ms:.2}ms ({durations:?})");
    assert!(
        median_ms < 5.0,
        "document_symbols must be <5ms, got {median_ms:.2}ms"
    );

    client.shutdown().await;
}

#[ignore = "requires AL_TEST_PROJECT_PATH environment variable"]
#[tokio::test]
async fn test_semantic_tokens_latency() {
    let mut client = LspClient::spawn(&test_project_dir()).await.unwrap();
    open_test_files(&mut client).await;

    // Warm up
    let _ = client
        .semantic_tokens("objects/API/ItemJournalStaging.Table.al")
        .await;

    let mut durations = Vec::with_capacity(ITERATIONS);
    for _ in 0..ITERATIONS {
        let start = Instant::now();
        let _ = client
            .semantic_tokens("objects/API/ItemJournalStaging.Table.al")
            .await;
        durations.push(start.elapsed().as_micros() as u64);
    }

    let median_ms = median(&mut durations) as f64 / 1000.0;
    eprintln!("semantic_tokens median: {median_ms:.2}ms ({durations:?})");
    assert!(
        median_ms < 15.0,
        "semantic_tokens must be <15ms, got {median_ms:.2}ms"
    );

    client.shutdown().await;
}
