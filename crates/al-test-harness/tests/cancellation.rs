//! `$/cancelRequest` regression tests.
//!
//! These tests exercise tower-lsp 0.20's automatic $/cancelRequest handling
//! and verify that synchronous queries remain responsive around cancellation.
//!
//! Run with:
//!   cargo test -p al-test-harness --test cancellation
//!
//! Tests are gated behind the `al-lsp` binary fixture like other E2E tests.

use al_test_harness::*;

const TEST_DOC: &str = r#"
codeunit 50100 "Cancellation Test"
{
    procedure Multiply(A: Integer; B: Integer): Integer
    begin
        exit(A * B);
    end;

    procedure Divide(A: Integer; B: Integer): Integer
    begin
        if B = 0 then
            Error('Division by zero');
        exit(A div B);
    end;
}
"#;

/// A cancellation for an unknown request ID must not disrupt the server.
#[tokio::test]
async fn cancel_non_existent_id_is_silently_ignored() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/cancel_test.al", TEST_DOC).await;

    client
        .cancel_request(99999)
        .await
        .expect("cancel notify should send");

    let symbols = client.document_symbols("src/cancel_test.al").await;
    assert!(
        !symbols.is_empty(),
        "server must remain responsive after cancelling a non-existent id"
    );

    client.shutdown().await;
}

/// Send $/cancelRequest before issuing the request with the matching ID.
/// Per tower-lsp 0.20 the cancel notification is queued; the next-issued
/// request with the matching id may resolve with a cancellation error or with
/// the result if the cancel was processed too late. Either is acceptable —
/// the contract is that the server never panics.
#[tokio::test]
async fn cancel_before_request_does_not_panic_server() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/cancel_test.al", TEST_DOC).await;

    let id = client.peek_next_request_id();
    client.cancel_request(id).await.expect("cancel notify");

    // Now issue document_symbols — its id will be `id` (the next assigned id
    // matches what we cancelled). Either it resolves normally or returns an
    // error; either way the server must not panic.
    let _ = client.document_symbols("src/cancel_test.al").await;

    let symbols = client.document_symbols("src/cancel_test.al").await;
    assert!(
        !symbols.is_empty(),
        "server must remain responsive after a pre-cancelled request"
    );

    client.shutdown().await;
}
