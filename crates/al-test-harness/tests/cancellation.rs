//! T052: $/cancelRequest regression tests.
//!
//! These tests exercise tower-lsp 0.20's automatic $/cancelRequest handling
//! plus the al-core spawn_blocking wraps added under T028 (acd547e, 2466093)
//! that make the heavy synchronous queries (references, document_symbol,
//! semantic_tokens) cancel-friendly.
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

/// T052: send $/cancelRequest for a non-existent id; the server must
/// gracefully ignore it (no panic, no protocol break).
#[tokio::test]
async fn cancel_non_existent_id_is_silently_ignored() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/cancel_test.al", TEST_DOC).await;

    // Cancel an id that has never been issued. The server must remain
    // functional.
    client
        .cancel_request(99999)
        .await
        .expect("cancel notify should send");

    // Server should still respond to a follow-up request.
    let symbols = client.document_symbols("src/cancel_test.al").await;
    assert!(
        !symbols.is_empty(),
        "server must remain responsive after cancelling a non-existent id"
    );

    client.shutdown().await;
}

/// T052: send $/cancelRequest BEFORE issuing the request with the matching id.
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

    // Server is still responsive.
    let symbols = client.document_symbols("src/cancel_test.al").await;
    assert!(
        !symbols.is_empty(),
        "server must remain responsive after a pre-cancelled request"
    );

    client.shutdown().await;
}
