//! Tests for the area A2 review fixes in the diagnostics publication.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use tower_lsp::{LanguageServer, LspService};
use tower_service::Service;

const BROKEN: &str = "codeunit 50100 Ghost\n{\n    procedure Broken(\n    begin\n    end;\n}\n";

/// A2-codex-07: a result is cached only when the URI's open state and version
/// did not change during the pass.
#[test]
fn project_result_is_current_needs_the_same_open_state_and_version() {
    assert!(project_result_is_current(None, None));
    assert!(project_result_is_current(Some(3), Some(3)));
    assert!(
        !project_result_is_current(Some(3), None),
        "closed during the pass"
    );
    assert!(
        !project_result_is_current(Some(3), Some(4)),
        "edited during the pass"
    );
    assert!(
        !project_result_is_current(None, Some(1)),
        "opened during the pass"
    );
}

/// A2-codex-08, A2-neon-06: an edit is applied while a project pass waits on
/// the client to take its notifications.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edit_is_not_blocked_by_workspace_publish() {
    let (mut service, mut socket) = LspService::new(crate::server::AlLsp::new);
    // The reader stops taking messages while `hold` is set, so a send blocks
    // until it is released, as with a client that is slow to read.
    let hold = Arc::new(AtomicBool::new(false));
    let release = Arc::new(tokio::sync::Notify::new());
    let reader = tokio::spawn({
        let (hold, release) = (Arc::clone(&hold), Arc::clone(&release));
        async move {
            use futures::StreamExt;
            loop {
                while hold.load(Ordering::Acquire) {
                    release.notified().await;
                }
                if socket.next().await.is_none() {
                    break;
                }
            }
        }
    });
    futures::future::poll_fn(|cx| service.poll_ready(cx))
        .await
        .unwrap();
    let request = tower_lsp::jsonrpc::Request::build("initialize")
        .params(serde_json::json!({ "capabilities": {} }))
        .id(1)
        .finish();
    let response = service.call(request).await.unwrap().unwrap();
    assert!(response.is_ok(), "{response:?}");
    let server = service.inner();

    let uris: Vec<Url> = (0..3)
        .map(|n| Url::parse(&format!("file:///proj/Ghost{n}.Codeunit.al")).unwrap())
        .collect();
    for uri in &uris {
        server
            .workspace
            .documents
            .open_with_client_version(uri.clone(), BROKEN.to_string(), 1)
            .unwrap();
        al_workspace::on_document_change(&server.workspace, uri, BROKEN);
    }
    hold.store(true, Ordering::Release);

    let pass = tokio::spawn({
        let workspace = Arc::clone(&server.workspace);
        let client = server.client.clone();
        let cache = Arc::clone(&server.semantic_diagnostic_cache);
        let published = Arc::clone(&server.workspace_diagnostic_uris);
        let session = server.session.clone();
        async move {
            publish_workspace_diagnostics_parts(workspace, client, cache, published, None, &session)
                .await
        }
    });
    // Wait until the pass is blocked inside its sends.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while server.workspace.diagnostic_publication.try_lock().is_ok() {
        assert!(
            std::time::Instant::now() < deadline,
            "the pass did not reach its sends"
        );
        assert!(!pass.is_finished(), "the pass finished without blocking");
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let edited = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        server.did_change(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: uris[0].clone(),
                version: 2,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: "codeunit 50100 Ghost\n{\n}\n".to_string(),
            }],
        }),
    )
    .await;

    hold.store(false, Ordering::Release);
    release.notify_one();
    assert!(
        edited.is_ok(),
        "did_change waited for the project pass's notifications"
    );
    assert_eq!(
        server.workspace.documents.get_client_version(&uris[0]),
        Some(2)
    );
    tokio::time::timeout(std::time::Duration::from_secs(10), pass)
        .await
        .expect("the pass finishes once the client reads")
        .unwrap();
    reader.abort();
}

/// A project pass that finds the scope set to `OpenFiles` sends nothing and
/// leaves `published_uris` alone. Set back to `Project`, the same pass publishes
/// and records the open document.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_project_pass_publishes_nothing_outside_project_scope() {
    let (mut service, mut socket) = LspService::new(crate::server::AlLsp::new);
    let hold = Arc::new(AtomicBool::new(false));
    let release = Arc::new(tokio::sync::Notify::new());
    let reader = tokio::spawn({
        let (hold, release) = (Arc::clone(&hold), Arc::clone(&release));
        async move {
            use futures::StreamExt;
            loop {
                while hold.load(Ordering::Acquire) {
                    release.notified().await;
                }
                if socket.next().await.is_none() {
                    break;
                }
            }
        }
    });
    futures::future::poll_fn(|cx| service.poll_ready(cx))
        .await
        .unwrap();
    let request = tower_lsp::jsonrpc::Request::build("initialize")
        .params(serde_json::json!({ "capabilities": {} }))
        .id(1)
        .finish();
    let response = service.call(request).await.unwrap().unwrap();
    assert!(response.is_ok(), "{response:?}");
    let server = service.inner();

    let uri = Url::parse("file:///proj/Ghost0.Codeunit.al").unwrap();
    server
        .workspace
        .documents
        .open_with_client_version(uri.clone(), BROKEN.to_string(), 1)
        .unwrap();
    al_workspace::on_document_change(&server.workspace, &uri, BROKEN);
    let session = server.session.clone();

    server.workspace.config.write().await.diagnostics_scope =
        al_project::config::DiagnosticsScope::OpenFiles;
    let published = publish_workspace_diagnostics_parts(
        Arc::clone(&server.workspace),
        server.client.clone(),
        Arc::clone(&server.semantic_diagnostic_cache),
        Arc::clone(&server.workspace_diagnostic_uris),
        None,
        &session,
    )
    .await;
    assert!(
        !published,
        "an OpenFiles scope must not publish a project pass"
    );
    assert!(server.workspace_diagnostic_uris.lock().await.is_empty());

    server.workspace.config.write().await.diagnostics_scope =
        al_project::config::DiagnosticsScope::Project;
    let published = publish_workspace_diagnostics_parts(
        Arc::clone(&server.workspace),
        server.client.clone(),
        Arc::clone(&server.semantic_diagnostic_cache),
        Arc::clone(&server.workspace_diagnostic_uris),
        None,
        &session,
    )
    .await;
    assert!(published, "the Project scope pass publishes");
    assert!(server.workspace_diagnostic_uris.lock().await.contains(&uri));
    reader.abort();
}
