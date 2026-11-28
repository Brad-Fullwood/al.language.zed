//! Tests for the area A2 review fixes of the language server lifecycle.

use super::*;

fn project(
    root: std::path::PathBuf,
    packages_dir: std::path::PathBuf,
) -> al_project::project::AlProject {
    al_project::project::AlProject {
        root,
        app_json: al_project::project::AppManifest {
            id: "00000000-0000-0000-0000-000000000001".to_string(),
            name: "Review test".to_string(),
            publisher: "Tests".to_string(),
            version: "1.0.0.0".to_string(),
            dependencies: Vec::new(),
            application: None,
            platform: None,
            runtime: None,
        },
        packages_dir,
        packages: Vec::new(),
        server_configs: Vec::new(),
        launch_config_error: None,
    }
}

/// A2-codex-06: `al.getStatus` answers while initialization has failed.
#[tokio::test]
async fn get_status_runs_in_the_failed_state() {
    let (service, _socket) = LspService::new(AlLsp::new);
    let server = service.inner();
    server
        .workspace_init_state
        .send_replace(WorkspaceInitState::Failed("x".to_string()));

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        server.execute_command(ExecuteCommandParams {
            command: "al.getStatus".to_string(),
            arguments: Vec::new(),
            work_done_progress_params: Default::default(),
        }),
    )
    .await
    .expect("al.getStatus does not wait for a ready workspace");

    assert!(matches!(result, Ok(Some(_))), "{result:?}");
}

/// A2-neon-09: a key removed from the editor settings goes back to its
/// start value.
#[tokio::test]
async fn removed_setting_returns_to_its_start_value() {
    let (service, mut socket) = LspService::new(AlLsp::new);
    // Each configuration change sends notifications to the client.
    tokio::spawn(async move {
        use futures::StreamExt;
        while socket.next().await.is_some() {}
    });
    let server = service.inner();
    *server.initial_al_settings.write().await =
        serde_json::json!({"diagnosticsScope": "openFiles"});
    server.workspace.config.write().await.diagnostics_scope =
        al_project::config::DiagnosticsScope::OpenFiles;
    server
        .workspace_init_state
        .send_replace(WorkspaceInitState::Ready);
    assert!(server.workspace.config.read().await.enable_code_actions);

    server
        .did_change_configuration(DidChangeConfigurationParams {
            settings: serde_json::json!({"al": {"enableCodeActions": false}}),
        })
        .await;
    assert!(!server.workspace.config.read().await.enable_code_actions);

    server
        .did_change_configuration(DidChangeConfigurationParams {
            settings: serde_json::json!({"al": {}}),
        })
        .await;
    assert!(server.workspace.config.read().await.enable_code_actions);
}

/// A2-neon-09: with no `initializationOptions`, a removed key goes back to
/// the default.
#[test]
fn staged_configuration_without_initialization_options_is_the_default() {
    let config = staged_configuration(&serde_json::Value::Null);
    assert_eq!(
        config.diagnostics_scope,
        al_project::config::DiagnosticsScope::Project
    );
    assert!(config.enable_code_actions);
}

/// A2-neon-09: the `initializationOptions` value is the baseline, not the
/// default.
#[tokio::test]
async fn removed_setting_returns_to_its_initialization_value() {
    let (service, _socket) = LspService::new(AlLsp::new);
    let server = service.inner();
    *server.initial_al_settings.write().await =
        serde_json::json!({"diagnosticsScope": "openFiles"});
    server
        .workspace_init_state
        .send_replace(WorkspaceInitState::Ready);

    server
        .did_change_configuration(DidChangeConfigurationParams {
            settings: serde_json::json!({"al": {}}),
        })
        .await;
    assert_eq!(
        server.workspace.config.read().await.diagnostics_scope,
        al_project::config::DiagnosticsScope::OpenFiles
    );
}

/// A2-neon-02: after a trust change clears the package cache path, the
/// project no longer names the revoked folder.
#[tokio::test]
async fn package_reload_drops_a_revoked_package_folder() {
    let (service, _socket) = LspService::new(AlLsp::new);
    let server = service.inner();
    let root = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    *server.workspace.project.write().await = Some(project(
        root.path().to_path_buf(),
        external.path().to_path_buf(),
    ));
    server.workspace.config.write().await.package_cache_path = None;

    server.reload_symbol_packages().await;

    let stored = server.workspace.project.read().await;
    let project = stored.as_ref().expect("project remains loaded");
    assert_eq!(project.packages_dir, root.path().join(".alpackages"));
    assert!(project
        .packages
        .iter()
        .all(|package| !package.starts_with(external.path())));
}

/// A2-neon-13: shutdown aborts a pending call graph pass.
#[tokio::test]
async fn shutdown_aborts_pending_transaction_tasks() {
    let (service, _socket) = LspService::new(AlLsp::new);
    let server = service.inner();
    let uri = Url::parse("file:///tmp/review/Pending.al").unwrap();
    let task = tokio::spawn(std::future::pending::<()>());
    let abort = task.abort_handle();
    server.transaction_tasks.lock().await.insert(uri, task);

    LanguageServer::shutdown(server).await.expect("shutdown");
    tokio::task::yield_now().await;

    assert!(abort.is_finished());
    assert!(server.transaction_tasks.lock().await.is_empty());
}

/// A2-neon-18: a save removes the debounced pass for the same file.
#[tokio::test]
async fn save_removes_the_pending_debounced_pass() {
    let (service, _socket) = LspService::new(AlLsp::new);
    let server = service.inner();
    let uri = Url::parse("file:///tmp/review/Saved.al").unwrap();
    let task = tokio::spawn(std::future::pending::<()>());
    let abort = task.abort_handle();
    server.diag_tasks.lock().await.insert(uri.clone(), task);

    server
        .did_save(DidSaveTextDocumentParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            text: None,
        })
        .await;
    tokio::task::yield_now().await;

    assert!(!server.diag_tasks.lock().await.contains_key(&uri));
    assert!(abort.is_finished());
}

/// A2-codex-05: with `documentChanges` declared, Sort Members carries the
/// buffer version it was computed on. Without it, the edit stays in `changes`.
#[tokio::test]
async fn sort_members_edit_carries_the_buffer_version() {
    let (service, _socket) = LspService::new(AlLsp::new);
    let server = service.inner();
    let uri = Url::parse("file:///tmp/review_a2_sort/Sort.al").unwrap();
    let source = "codeunit 50100 \"Sort\"\n{\n    procedure Zebra()\n    begin\n    end;\n\n    procedure Apple()\n    begin\n    end;\n}\n";
    server
        .workspace
        .documents
        .open_with_client_version(uri.clone(), source.to_string(), 7)
        .unwrap();
    let sort_edit = |server: &AlServer| {
        crate::server::handlers::handle_code_action(
            server,
            &uri,
            Range::new(Position::new(0, 0), Position::new(0, 0)),
            &[],
            None,
        )
        .unwrap_or_default()
        .into_iter()
        .find_map(|action| match action {
            CodeActionOrCommand::CodeAction(action) if action.title == "AL: Sort Members" => {
                action.edit
            }
            _ => None,
        })
        .expect("Sort Members is offered")
    };

    let unversioned = sort_edit(server);
    assert!(unversioned.document_changes.is_none());
    assert!(unversioned
        .changes
        .is_some_and(|changes| changes.contains_key(&uri)));

    server.versioned_edits.store(true, Ordering::Relaxed);
    let versioned = sort_edit(server);
    assert!(versioned.changes.is_none());
    let Some(DocumentChanges::Edits(edits)) = versioned.document_changes else {
        panic!("documentChanges holds text document edits");
    };
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].text_document.uri, uri);
    assert_eq!(edits[0].text_document.version, Some(7));
}

/// A2-codex-14, A2-neon-08: `al.enableCodeActions` off removes every action,
/// including the object actions on a declaration line.
#[tokio::test]
async fn disabled_code_actions_offer_nothing_on_an_object_line() {
    let (service, _socket) = LspService::new(AlLsp::new);
    let server = service.inner();
    let uri = Url::parse("file:///tmp/review_a2_disabled/Sort.al").unwrap();
    let source = "codeunit 50100 \"Sort\"\n{\n    procedure Zebra()\n    begin\n    end;\n\n    procedure Apple()\n    begin\n    end;\n}\n";
    server
        .workspace
        .documents
        .open_with_client_version(uri.clone(), source.to_string(), 1)
        .unwrap();
    let request = || {
        crate::server::handlers::handle_code_action(
            server,
            &uri,
            Range::new(Position::new(0, 0), Position::new(0, 0)),
            &[],
            None,
        )
    };
    assert!(request().is_some_and(|actions| !actions.is_empty()));
    server.workspace.config.write().await.enable_code_actions = false;
    assert!(request().is_none());
}
