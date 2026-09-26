//! Unit tests for the LSP server, one module per concern.

use super::*;

mod session_lifecycle_tests {
    use super::*;

    #[test]
    fn cancellation_is_shared_by_every_session_clone() {
        let session = LspSessionState::default();
        let background_task = session.clone();

        assert!(!background_task.is_cancelled());
        session.cancel();
        assert!(background_task.is_cancelled());
    }

    #[tokio::test]
    async fn language_server_shutdown_cancels_background_transport_access() {
        let (service, _socket) = LspService::new(AlServer::new);
        let server = service.inner();

        assert!(!server.session.is_cancelled());
        LanguageServer::shutdown(server)
            .await
            .expect("shutdown should cancel an idle server");
        assert!(server.session.is_cancelled());
    }
}

mod symbol_package_configuration_tests {
    use super::*;

    fn project(root: std::path::PathBuf) -> al_project::project::AlProject {
        al_project::project::AlProject {
            root: root.clone(),
            app_json: al_project::project::AppManifest {
                id: "00000000-0000-0000-0000-000000000001".to_string(),
                name: "Configuration test".to_string(),
                publisher: "Tests".to_string(),
                version: "1.0.0.0".to_string(),
                dependencies: Vec::new(),
                application: None,
                platform: None,
                runtime: None,
            },
            packages_dir: root.join(".alpackages"),
            packages: Vec::new(),
            server_configs: Vec::new(),
            launch_config_error: None,
        }
    }

    #[tokio::test]
    async fn configuration_reload_replaces_package_cache_and_local_folder_paths() {
        let (service, _socket) = LspService::new(AlServer::new);
        let server = service.inner();
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("symbols-cache");
        let local = root.path().join("shared-symbols");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::create_dir_all(&local).unwrap();
        *server.workspace.project.write().await = Some(project(root.path().to_path_buf()));
        // `packageCachePath` is privileged, so the trust gate needs the project
        // root to ask whether the repository is what asked for it. This one is
        // the user's own setting: the project carries no settings file.
        *server.root_uri.write().await = Some(Url::from_directory_path(root.path()).unwrap());
        server
            .workspace_init_state
            .send_replace(WorkspaceInitState::Ready);

        server
            .did_change_configuration(DidChangeConfigurationParams {
                settings: serde_json::json!({
                    "al": {
                        "packageCachePath": "symbols-cache",
                        "appLocalFolderPaths": ["shared-symbols"]
                    }
                }),
            })
            .await;

        let stored = server.workspace.project.read().await;
        let project = stored.as_ref().expect("project remains loaded");
        assert_eq!(project.packages_dir, cache);
        assert!(project.packages.is_empty());
        let config = server.workspace.config.read().await;
        assert_eq!(
            config.package_cache_path.as_deref(),
            Some(std::path::Path::new("symbols-cache"))
        );
        assert_eq!(
            config.app_local_folder_paths,
            [std::path::PathBuf::from("shared-symbols")]
        );
    }

    #[tokio::test]
    async fn rejected_package_reload_retains_previous_config_project_and_symbols() {
        let (service, _socket) = LspService::new(AlServer::new);
        let server = service.inner();
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("symbols-cache");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("Broken.app"), b"not an app").unwrap();
        let original_project = project(root.path().to_path_buf());
        let original_packages_dir = original_project.packages_dir.clone();
        *server.workspace.project.write().await = Some(original_project);
        server
            .workspace
            .symbols
            .add_entries(&[al_symbols::SymbolEntry {
                kind: al_symbols::ObjectKind::Codeunit,
                id: 50_100,
                name: "Stable".to_string(),
                package: "Previous".to_string(),
                ..Default::default()
            }]);
        server
            .workspace_init_state
            .send_replace(WorkspaceInitState::Ready);

        server
            .did_change_configuration(DidChangeConfigurationParams {
                settings: serde_json::json!({
                    "al": {
                        "packageCachePath": "symbols-cache"
                    }
                }),
            })
            .await;

        let project = server.workspace.project.read().await;
        assert_eq!(
            project.as_ref().unwrap().packages_dir,
            original_packages_dir
        );
        assert!(server
            .workspace
            .config
            .read()
            .await
            .package_cache_path
            .is_none());
        assert!(server.workspace.symbols.find_by_name("Stable").is_some());
    }
}

mod document_close_generation_tests {
    use super::*;

    #[tokio::test]
    async fn close_restores_saved_source_instead_of_leaving_unsaved_overlay() {
        let (service, _socket) = LspService::new(AlServer::new);
        let server = service.inner();
        server.workspace.config.write().await.diagnostics_scope =
            al_project::config::DiagnosticsScope::OpenFiles;

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("Saved.al");
        let saved = r#"codeunit 50100 "Saved" { }"#;
        let unsaved = r#"codeunit 50100 "Unsaved" { }"#;
        std::fs::write(&path, saved).unwrap();
        let uri = Url::from_file_path(&path).unwrap();
        server
            .workspace
            .documents
            .open(uri.clone(), unsaved.to_string())
            .unwrap();
        server
            .workspace
            .file_index
            .add_file(path.clone(), unsaved.to_string());

        server
            .did_close(DidCloseTextDocumentParams {
                text_document: TextDocumentIdentifier { uri },
            })
            .await;

        assert_eq!(
            server.workspace.file_index.get_content(&path).as_deref(),
            Some(saved)
        );
        assert!(server
            .workspace
            .file_index
            .find_by_object_name("Saved")
            .is_some());
        assert!(server
            .workspace
            .file_index
            .find_by_object_name("Unsaved")
            .is_none());
    }

    #[tokio::test]
    async fn close_removes_a_never_saved_transient_document() {
        let (service, _socket) = LspService::new(AlServer::new);
        let server = service.inner();
        server.workspace.config.write().await.diagnostics_scope =
            al_project::config::DiagnosticsScope::OpenFiles;

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("Transient.al");
        let uri = Url::from_file_path(&path).unwrap();
        let text = r#"codeunit 50100 "Transient" { }"#;
        server
            .workspace
            .documents
            .open(uri.clone(), text.to_string())
            .unwrap();
        server
            .workspace
            .file_index
            .add_file(path.clone(), text.to_string());

        server
            .did_close(DidCloseTextDocumentParams {
                text_document: TextDocumentIdentifier { uri },
            })
            .await;

        assert!(server.workspace.file_index.get_content(&path).is_none());
        assert!(server
            .workspace
            .file_index
            .find_by_object_name("Transient")
            .is_none());
    }
}

mod runnables_tests {
    use super::*;
    use al_analysis::queries::tests::{TestCodeunit, TestProcedure};

    #[test]
    fn emits_zed_shell_runnables_for_project_file_and_discovered_tests() {
        let root = tempfile::tempdir().expect("temporary project");
        let file = root.path().join("CustomerTests.Codeunit.AL");
        std::fs::write(&file, "codeunit 50100 CustomerTests {}").expect("test file");
        let explorer = root.path().join(if cfg!(windows) {
            "al-explorer.exe"
        } else {
            "al-explorer"
        });
        let uri = Url::from_file_path(&file).expect("file URI");
        let tests = vec![
            TestCodeunit {
                name: "Customer Tests".to_string(),
                id: 50100,
                file: file.to_string_lossy().into_owned(),
                tests: vec![TestProcedure {
                    name: "CreatesCustomer".to_string(),
                    line: 7,
                    handler_functions: Vec::new(),
                }],
                test_initializers: Vec::new(),
                test_cleanups: Vec::new(),
            },
            TestCodeunit {
                name: "Other Tests".to_string(),
                id: 50101,
                file: root
                    .path()
                    .join("OtherTests.Codeunit.al")
                    .to_string_lossy()
                    .into_owned(),
                tests: vec![TestProcedure {
                    name: "DoesNotBelongToThisFile".to_string(),
                    line: 3,
                    handler_functions: Vec::new(),
                }],
                test_initializers: Vec::new(),
                test_cleanups: Vec::new(),
            },
        ];

        let runnables = build_runnables(&uri, &file, root.path(), &explorer, true, &tests, None);
        assert_eq!(runnables.len(), 3);
        assert_eq!(runnables[0].label, "AL: Compile project");
        assert_eq!(
            runnables[0].args.args,
            [
                "compile",
                "--project",
                root.path().to_string_lossy().as_ref()
            ]
        );
        assert_eq!(runnables[1].label, "AL: Lint current file");
        assert_eq!(
            runnables[1].args.args,
            ["lint", file.to_string_lossy().as_ref()]
        );
        assert_eq!(
            runnables[2].label,
            "AL: Test Customer Tests.CreatesCustomer"
        );
        assert_eq!(
            runnables[2].args.args,
            [
                "test-run",
                "50100",
                "--name",
                "Customer Tests",
                "--method",
                "CreatesCustomer"
            ]
        );
        let location = runnables[2].location.as_ref().expect("test location");
        assert_eq!(location.target_selection_range.start.line, 6);

        let json = serde_json::to_value(&runnables[2]).expect("runnable JSON");
        assert_eq!(json["kind"], "shell");
        assert_eq!(json["args"]["environment"], serde_json::json!({}));
        assert_eq!(json["args"]["program"], explorer.to_string_lossy().as_ref());
        assert_eq!(json["args"]["cwd"], root.path().to_string_lossy().as_ref());
    }

    #[test]
    fn position_selects_only_the_test_under_the_cursor() {
        let root = tempfile::tempdir().expect("temporary project");
        let file = root.path().join("CustomerTests.Codeunit.al");
        std::fs::write(&file, "codeunit 50100 CustomerTests {}").expect("test file");
        let explorer = root.path().join("al-explorer");
        let uri = Url::from_file_path(&file).expect("file URI");
        let tests = vec![TestCodeunit {
            name: "Customer Tests".to_string(),
            id: 50100,
            file: file.to_string_lossy().into_owned(),
            tests: vec![
                TestProcedure {
                    name: "First".to_string(),
                    line: 5,
                    handler_functions: Vec::new(),
                },
                TestProcedure {
                    name: "Second".to_string(),
                    line: 20,
                    handler_functions: Vec::new(),
                },
            ],
            test_initializers: Vec::new(),
            test_cleanups: Vec::new(),
        }];

        // Cursor inside the body of the second test (0-based line 25).
        let at_second = build_runnables(
            &uri,
            &file,
            root.path(),
            &explorer,
            false,
            &tests,
            Some(Position {
                line: 25,
                character: 4,
            }),
        );
        let labels: Vec<&str> = at_second.iter().map(|r| r.label.as_str()).collect();
        assert!(
            labels.contains(&"AL: Test Customer Tests.Second"),
            "cursor in the second test must offer it: {labels:?}"
        );
        assert!(
            !labels.contains(&"AL: Test Customer Tests.First"),
            "a positioned request must not return every test in the file: {labels:?}"
        );

        // Without a position the whole file's tests are listed.
        let all = build_runnables(&uri, &file, root.path(), &explorer, false, &tests, None);
        let labels: Vec<&str> = all.iter().map(|r| r.label.as_str()).collect();
        assert!(
            labels.contains(&"AL: Test Customer Tests.First"),
            "{labels:?}"
        );
        assert!(
            labels.contains(&"AL: Test Customer Tests.Second"),
            "{labels:?}"
        );
    }

    #[test]
    fn cursor_above_every_test_offers_no_test_runnable() {
        let root = tempfile::tempdir().expect("temporary project");
        let file = root.path().join("T.Codeunit.al");
        std::fs::write(&file, "codeunit 50100 T {}").expect("test file");
        let uri = Url::from_file_path(&file).expect("file URI");
        let tests = vec![TestCodeunit {
            name: "T".to_string(),
            id: 50100,
            file: file.to_string_lossy().into_owned(),
            tests: vec![TestProcedure {
                name: "Only".to_string(),
                line: 10,
                handler_functions: Vec::new(),
            }],
            test_initializers: Vec::new(),
            test_cleanups: Vec::new(),
        }];
        let runnables = build_runnables(
            &uri,
            &file,
            root.path(),
            std::path::Path::new("/tools/al-explorer"),
            false,
            &tests,
            Some(Position {
                line: 1,
                character: 0,
            }),
        );
        assert_eq!(
            runnables.len(),
            1,
            "only the file-level lint runnable remains: {runnables:?}"
        );
        assert_eq!(runnables[0].label, "AL: Lint current file");
    }

    #[test]
    fn omits_project_compile_runnable_without_a_project() {
        let runnables = build_runnables(
            &Url::parse("file:///tmp/Standalone.al").unwrap(),
            std::path::Path::new("/tmp/Standalone.al"),
            std::path::Path::new("/tmp"),
            std::path::Path::new("/tools/al-explorer"),
            false,
            &[],
            None,
        );
        assert_eq!(runnables.len(), 1);
        assert_eq!(runnables[0].label, "AL: Lint current file");
    }
}

mod diagnostics_debounce_tests {
    //! The debounce machinery is per document. A single shared slot let an
    //! edit in file B abort file A's pending run, leaving A with stale
    //! squiggles until its next change or save.

    use super::*;

    async fn server_with_documents(sources: &[(&str, &str)]) -> (LspService<AlServer>, Vec<Url>) {
        let (service, _socket) = LspService::new(AlServer::new);
        let server = service.inner();
        server
            .workspace_init_state
            .send_replace(WorkspaceInitState::Ready);
        let mut uris = Vec::new();
        for (path, text) in sources {
            let uri = Url::parse(path).expect("valid uri");
            server
                .workspace
                .documents
                .open_with_client_version(uri.clone(), (*text).to_string(), 1)
                .expect("document opens");
            uris.push(uri);
        }
        (service, uris)
    }

    #[tokio::test]
    async fn scheduling_one_document_does_not_cancel_another() {
        let (service, uris) = server_with_documents(&[
            (
                "file:///proj/A.Codeunit.al",
                "codeunit 50100 A
{
    procedure P()
    begin
    end;
}
",
            ),
            (
                "file:///proj/B.Codeunit.al",
                "codeunit 50101 B
{
    procedure Q()
    begin
    end;
}
",
            ),
        ])
        .await;
        let server = service.inner();

        // Edit A, then edit B within A's debounce window.
        server.schedule_diagnostics(uris[0].clone()).await;
        server.schedule_diagnostics(uris[1].clone()).await;

        let (a_task, b_task) = {
            let mut tasks = server.diag_tasks.lock().await;
            assert_eq!(
                tasks.len(),
                2,
                "each document must own its own pending debounce task"
            );
            (
                tasks.remove(&uris[0]).expect("A has a pending task"),
                tasks.remove(&uris[1]).expect("B has a pending task"),
            )
        };

        let a_result = a_task.await;
        let b_result = b_task.await;
        assert!(
            a_result.is_ok(),
            "editing B must not cancel A's pending diagnostics: {a_result:?}"
        );
        assert!(b_result.is_ok(), "{b_result:?}");
    }

    #[tokio::test]
    async fn rescheduling_the_same_document_keeps_exactly_one_task() {
        let (service, uris) = server_with_documents(&[(
            "file:///proj/A.Codeunit.al",
            "codeunit 50100 A
{
}
",
        )])
        .await;
        let server = service.inner();

        for _ in 0..3 {
            server.schedule_diagnostics(uris[0].clone()).await;
        }
        let tasks = server.diag_tasks.lock().await;
        assert_eq!(
            tasks.len(),
            1,
            "repeated keystrokes on one document must collapse to one task"
        );
    }

    #[tokio::test]
    async fn closing_a_document_cancels_only_its_own_task() {
        let (service, uris) = server_with_documents(&[
            (
                "file:///proj/A.Codeunit.al",
                "codeunit 50100 A
{
}
",
            ),
            (
                "file:///proj/B.Codeunit.al",
                "codeunit 50101 B
{
}
",
            ),
        ])
        .await;
        let server = service.inner();
        server.schedule_diagnostics(uris[0].clone()).await;
        server.schedule_diagnostics(uris[1].clone()).await;

        server
            .did_close(DidCloseTextDocumentParams {
                text_document: TextDocumentIdentifier {
                    uri: uris[1].clone(),
                },
            })
            .await;

        let mut tasks = server.diag_tasks.lock().await;
        assert!(
            tasks.contains_key(&uris[0]),
            "the other document's pending run must survive a close"
        );
        assert!(
            !tasks.contains_key(&uris[1]),
            "the closed document's pending run must be cancelled"
        );
        let a_task = tasks.remove(&uris[0]).expect("A still pending");
        drop(tasks);
        assert!(
            a_task.await.is_ok(),
            "closing B must not abort A's diagnostics"
        );
    }

    #[tokio::test]
    async fn cache_uris_are_never_scheduled() {
        let (service, _uris) = server_with_documents(&[]).await;
        let server = service.inner();
        let cache_uri =
            Url::from_file_path(al_symbols::virtual_file::cache_dir().join("Base.Application.al"))
                .expect("cache uri");
        server.schedule_diagnostics(cache_uri).await;
        assert!(
            server.diag_tasks.lock().await.is_empty(),
            "virtual symbol-cache files must not get debounced diagnostics"
        );
    }
}

mod workspace_init_state_tests {
    use super::*;

    #[tokio::test]
    async fn ready_state_releases_requests() {
        let (service, _socket) = LspService::new(AlServer::new);
        service
            .inner()
            .workspace_init_state
            .send_replace(WorkspaceInitState::Ready);
        let _generation = service
            .inner()
            .await_ready()
            .await
            .expect("ready workspace");
    }

    #[tokio::test]
    async fn failed_state_returns_the_retained_initialization_error() {
        let (service, _socket) = LspService::new(AlServer::new);
        service
            .inner()
            .workspace_init_state
            .send_replace(WorkspaceInitState::Failed(
                "configured package directory is unreadable".to_string(),
            ));

        let error = service
            .inner()
            .await_ready()
            .await
            .expect_err("failed initialization must fail requests");
        assert_eq!(error.code, tower_lsp::jsonrpc::ErrorCode::InternalError);
        assert!(error
            .message
            .contains("configured package directory is unreadable"));
    }

    #[tokio::test]
    async fn semantic_phase_waits_for_the_initialized_workspace_generation() {
        let (service, _socket) = LspService::new(AlServer::new);
        let state = service.inner().workspace_init_state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            state.send_replace(WorkspaceInitState::Ready);
        });

        service
            .inner()
            .await_semantic_workspace()
            .await
            .expect("semantic phase should resume after workspace readiness");
    }
}

mod trust_gate_tests {
    use super::*;

    fn privileged_config() -> al_project::config::AlConfig {
        al_project::config::AlConfig {
            code_analyzers: vec!["${CodeCop}".to_string(), "./tools/Payload.dll".to_string()],
            compilation_options: vec!["/analyzer:/tmp/x.dll".to_string()],
            ..Default::default()
        }
    }

    /// The settings the editor sends already carry whatever the worktree's
    /// `.zed/settings.json` contributed. With no root there is no repository to
    /// subtract, so applying them whole would apply the repository's.
    #[tokio::test]
    async fn no_root_uri_denies_every_privileged_setting() {
        let mut config = privileged_config();

        let advisory = gate_repository_settings(None, &mut config);

        assert_eq!(
            config.code_analyzers,
            vec!["${CodeCop}".to_string()],
            "an analyzer path must not survive a session with no project root"
        );
        assert!(config.compilation_options.is_empty());
        let advisory = advisory.expect("the user is told why their settings were dropped");
        assert!(
            advisory.contains("no local project directory"),
            "{advisory}"
        );
    }

    /// A non-`file:` root is the same situation: nothing local to read.
    #[tokio::test]
    async fn a_non_file_root_denies_every_privileged_setting() {
        let mut config = privileged_config();
        let root = Url::parse("untitled:workspace").expect("valid uri");

        let advisory = gate_repository_settings(Some(&root), &mut config);

        assert_eq!(config.code_analyzers, vec!["${CodeCop}".to_string()]);
        assert!(advisory.is_some_and(|text| text.contains("no local project directory")));
    }
}

mod generation_guard_tests {
    use super::*;

    /// A `references`-shaped request used to hold the generation read guard for
    /// the whole blocking walk, so the next keystroke blocked in `did_change`'s
    /// `generation_lock.write()` until the walk finished. The edit must land
    /// while the slow request is still running.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_edit_lands_while_a_slow_request_is_in_flight() {
        let (service, _socket) = LspService::new(AlServer::new);
        let server = service.inner();
        server
            .workspace_init_state
            .send_replace(WorkspaceInitState::Ready);

        let uri = Url::parse("file:///proj/Foo.Codeunit.al").unwrap();
        server
            .workspace
            .documents
            .open(uri.clone(), "codeunit 50100 Foo\n{\n}\n".to_string())
            .unwrap();

        let (release, blocked) = std::sync::mpsc::channel::<()>();
        // Only the first pass waits for the edit. The edit moves the
        // generation, so `offload_after_ready` recomputes, and the recompute
        // must not block on a channel nobody sends to again.
        let gate = std::sync::Mutex::new(Some(blocked));
        let slow = server.offload_after_ready("slow", move || {
            let first_pass = gate.lock().expect("gate is never poisoned").take();
            if let Some(blocked) = first_pass {
                blocked.recv().expect("the edit releases the slow worker");
            }
        });

        let edit = async {
            let params = DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: uri.clone(),
                    version: 2,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: "codeunit 50100 Foo\n{\n    procedure X() begin end;\n}\n".to_string(),
                }],
            };
            server.did_change(params).await;
            release.send(()).expect("slow worker is still waiting");
        };

        let (slow_result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(slow, edit)
        })
        .await
        .expect("did_change must not wait for the slow request");

        assert_eq!(
            server
                .workspace
                .documents
                .get_text_and_client_version(&uri)
                .expect("document stays open")
                .1,
            2,
            "the edit must have been applied"
        );
        slow_result.expect("the request is recomputed against the new generation, not failed");
    }

    /// `did_close` bumps the generation twice: once on the close, once when the
    /// saved file has been read back from disk. A `documentSymbol` or
    /// `workspace/symbol` issued right after a close landed on the second bump
    /// and came back `ContentModified`, so closing a file broke reads of every
    /// other file.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unrelated_generation_bump_is_recomputed_not_reported() {
        let (service, _socket) = LspService::new(AlServer::new);
        let server = service.inner();
        server
            .workspace_init_state
            .send_replace(WorkspaceInitState::Ready);

        let passes = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let counter = Arc::clone(&passes);
        let workspace = Arc::clone(&server.workspace);
        let result = server
            .offload_after_ready("read", move || {
                if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    // The first pass runs across a generation swap, the way a
                    // did_close disk restore lands under an in-flight read.
                    workspace.mark_generation_changed();
                }
                "symbols"
            })
            .await;

        assert_eq!(
            result.expect("an unrelated generation swap must not fail a read request"),
            "symbols"
        );
        assert_eq!(
            passes.load(Ordering::SeqCst),
            2,
            "the invalidated pass is recomputed against the new generation"
        );
    }

    /// Typing does not stop for a read request. Once the bounded recompute is
    /// spent the newest result is returned, because a read request that never
    /// answers is worse than one answered from a generation old by a keystroke.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_read_request_answers_while_the_workspace_keeps_churning() {
        let (service, _socket) = LspService::new(AlServer::new);
        let server = service.inner();
        server
            .workspace_init_state
            .send_replace(WorkspaceInitState::Ready);

        let passes = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let counter = Arc::clone(&passes);
        let workspace = Arc::clone(&server.workspace);
        let result = server
            .offload_after_ready("read", move || {
                counter.fetch_add(1, Ordering::SeqCst);
                workspace.mark_generation_changed();
                "symbols"
            })
            .await;

        assert_eq!(
            result.expect("a churning workspace must not fail a read request"),
            "symbols"
        );
        assert_eq!(
            passes.load(Ordering::SeqCst),
            MAX_OFFLOAD_ATTEMPTS,
            "the recompute is bounded"
        );
    }
}

mod implementation_capability_tests {
    use super::*;

    #[tokio::test]
    async fn native_lsp_advertises_and_serves_go_to_implementation() {
        let (service, _socket) = LspService::new(AlServer::new);
        let server = service.inner();
        server
            .workspace_init_state
            .send_replace(WorkspaceInitState::Ready);

        let interface_uri = Url::parse("file:///proj/IFoo.Interface.al").unwrap();
        server
            .workspace
            .documents
            .open(
                interface_uri.clone(),
                "interface 50100 IFoo\n{\n    procedure Run();\n}\n".to_string(),
            )
            .unwrap();
        server.workspace.file_index.add_file(
            std::path::PathBuf::from("/proj/FooImpl.Codeunit.al"),
            "codeunit 50101 FooImpl implements IFoo\n{\n    procedure Run()\n    begin\n    end;\n}\n"
                .to_string(),
        );

        let initialized = server
            .initialize(InitializeParams::default())
            .await
            .expect("initialize succeeds");
        assert_eq!(
            initialized.capabilities.implementation_provider,
            Some(ImplementationProviderCapability::Simple(true))
        );

        let response = server
            .goto_implementation(GotoImplementationParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: interface_uri },
                    position: Position {
                        line: 0,
                        character: "interface 50100 ".len() as u32,
                    },
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .await
            .expect("go-to-implementation succeeds")
            .expect("implementation exists");

        let locations = match response {
            GotoImplementationResponse::Scalar(location) => vec![location],
            GotoImplementationResponse::Array(locations) => locations,
            GotoImplementationResponse::Link(_) => {
                panic!("the native handler returns locations, not location links")
            }
        };
        assert_eq!(locations.len(), 1);
        assert!(locations[0].uri.path().ends_with("/FooImpl.Codeunit.al"));
    }
}

mod document_symbol_capability_tests {
    //! `document_symbol` must honour the client's
    //! `hierarchicalDocumentSymbolSupport` capability: nested `DocumentSymbol[]`
    //! when advertised, flat `SymbolInformation[]` otherwise. These drive the
    //! real handler in-process (no transport) through `initialize`, so they
    //! cover both the capability capture and the response-shape branch.

    use super::*;

    const SRC: &str =
        "codeunit 50100 \"Outline CU\"\n{\n    procedure DoWork()\n    begin\n    end;\n}\n";

    /// Build an in-process server with the document open and `await_ready`
    /// short-circuited, then run `initialize` with the given client capabilities.
    async fn server_after_initialize(caps: ClientCapabilities) -> (LspService<AlServer>, Url) {
        let (service, _socket) = LspService::new(AlServer::new);
        let server = service.inner();
        // Skip the 30s workspace-init wait in `await_ready`.
        server
            .workspace_init_state
            .send_replace(WorkspaceInitState::Ready);
        let uri = Url::parse("file:///proj/Outline.al").expect("valid uri");
        server
            .workspace
            .documents
            .open(uri.clone(), SRC.to_string())
            .unwrap();
        server
            .initialize(InitializeParams {
                capabilities: caps,
                ..Default::default()
            })
            .await
            .expect("initialize succeeds");
        (service, uri)
    }

    fn ds_params(uri: &Url) -> DocumentSymbolParams {
        DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        }
    }

    #[tokio::test]
    async fn nested_when_client_advertises_hierarchical_support() {
        let caps = ClientCapabilities {
            text_document: Some(TextDocumentClientCapabilities {
                document_symbol: Some(DocumentSymbolClientCapabilities {
                    hierarchical_document_symbol_support: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let (service, uri) = server_after_initialize(caps).await;

        match service.inner().document_symbol(ds_params(&uri)).await {
            Ok(Some(DocumentSymbolResponse::Nested(syms))) => {
                let object = &syms[0];
                let children = object
                    .children
                    .as_ref()
                    .expect("object with a procedure has nested children");
                assert!(
                    children.iter().any(|c| c.name.contains("DoWork")),
                    "nested child procedure should be present, got {children:?}"
                );
            }
            other => panic!("expected Nested response, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn flat_when_client_omits_hierarchical_support() {
        // Empty `documentSymbol` capability == no hierarchical support advertised.
        let caps = ClientCapabilities {
            text_document: Some(TextDocumentClientCapabilities {
                document_symbol: Some(DocumentSymbolClientCapabilities::default()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let (service, uri) = server_after_initialize(caps).await;

        match service.inner().document_symbol(ds_params(&uri)).await {
            Ok(Some(DocumentSymbolResponse::Flat(infos))) => {
                // The procedure is flattened out with its object as container.
                let proc = infos
                    .iter()
                    .find(|s| s.name.contains("DoWork"))
                    .expect("procedure present in flat list");
                assert_eq!(
                    proc.container_name.as_deref(),
                    Some("Outline CU"),
                    "flattened child records its parent object as container_name"
                );
            }
            other => panic!("expected Flat response, got {other:?}"),
        }
    }
}

mod definition_link_support_tests {
    //! `goto_definition` must honour the client's `definition.linkSupport`
    //! capability: `LocationLink[]` when advertised, plain `Location[]`
    //! otherwise. Driven in-process through `initialize`, so they cover both the
    //! capability capture and the response-shape branch. (The integration
    //! harness only exercises the `Location[]` side.)

    use super::*;

    // A local variable used after its declaration — go-to-definition on the use
    // resolves intra-file to the declaration, needing only an open document.
    const SRC: &str = "codeunit 50100 \"Test\"\n{\n    procedure Foo()\n    var\n        MyVar: Integer;\n    begin\n        MyVar := 42;\n    end;\n}\n";

    async fn server_after_initialize(caps: ClientCapabilities) -> (LspService<AlServer>, Url) {
        let (service, _socket) = LspService::new(AlServer::new);
        let server = service.inner();
        server
            .workspace_init_state
            .send_replace(WorkspaceInitState::Ready);
        let uri = Url::parse("file:///proj/Def.al").expect("valid uri");
        server
            .workspace
            .documents
            .open(uri.clone(), SRC.to_string())
            .unwrap();
        server
            .initialize(InitializeParams {
                capabilities: caps,
                ..Default::default()
            })
            .await
            .expect("initialize succeeds");
        (service, uri)
    }

    fn def_params(uri: &Url) -> GotoDefinitionParams {
        // The `MyVar` use on line 6 (0-based), column 8.
        GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                position: Position {
                    line: 6,
                    character: 8,
                },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        }
    }

    #[tokio::test]
    async fn link_response_when_client_advertises_link_support() {
        let caps = ClientCapabilities {
            text_document: Some(TextDocumentClientCapabilities {
                definition: Some(GotoCapability {
                    link_support: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let (service, uri) = server_after_initialize(caps).await;

        match service.inner().goto_definition(def_params(&uri)).await {
            Ok(Some(GotoDefinitionResponse::Link(links))) => {
                assert!(!links.is_empty(), "expected at least one LocationLink");
            }
            other => panic!("expected Link response, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn array_response_when_client_omits_link_support() {
        // Empty `definition` capability == no linkSupport advertised.
        let caps = ClientCapabilities {
            text_document: Some(TextDocumentClientCapabilities {
                definition: Some(GotoCapability::default()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let (service, uri) = server_after_initialize(caps).await;

        match service.inner().goto_definition(def_params(&uri)).await {
            Ok(Some(GotoDefinitionResponse::Array(locs))) => {
                assert!(!locs.is_empty(), "expected at least one Location");
            }
            other => panic!("expected Array response, got {other:?}"),
        }
    }
}

mod code_lens_command_wiring_tests {
    //! every CodeLens the server emits must resolve to an
    //! `executeCommand` handler — a clicked lens must perform its action, never
    //! a silent no-op. These tests drive the real `code_lens` + `execute_command`
    //! handlers in-process (no transport) and assert both the structural
    //! invariant (`LENS_COMMAND_IDS ⊆ SUPPORTED_COMMANDS`) and end-to-end that
    //! each emitted lens is dispatched (returns `Some`, not the catch-all's
    //! `None`).

    use super::*;
    use al_analysis::queries::code_lens::LENS_COMMAND_IDS;
    use al_analysis::queries::profiler_hints::{ProfilerHint, ProfilerSession};

    // A test codeunit exercising all three lens kinds: TestBeta is called once
    // (reference lens), both methods are `[Test]` (test lenses), and a profiler
    // session below adds a profiler lens for TestAlpha.
    const SRC: &str = "codeunit 50200 \"My Tests\"\n{\n    Subtype = Test;\n\n    [Test]\n    procedure TestAlpha()\n    begin\n        TestBeta();\n    end;\n\n    [Test]\n    procedure TestBeta()\n    begin\n    end;\n}\n";

    fn build_server() -> (LspService<AlServer>, Url) {
        let (service, _socket) = LspService::new(AlServer::new);
        let server = service.inner();
        // Skip the workspace-init wait in `await_ready`.
        server
            .workspace_init_state
            .send_replace(WorkspaceInitState::Ready);
        let uri = Url::parse("file:///proj/MyTests.al").expect("valid uri");
        server
            .workspace
            .documents
            .open(uri.clone(), SRC.to_string())
            .unwrap();
        (service, uri)
    }

    fn add_profiler_session(server: &AlServer, uri: &Url, procedure: &str) {
        let file_path = uri.to_file_path().unwrap().to_string_lossy().to_string();
        let hint = ProfilerHint {
            procedure: procedure.to_string(),
            object: "My Tests".to_string(),
            self_time_ms: 42.0,
            total_time_ms: 42.0,
            hit_count: 3,
            file: Some(file_path.clone()),
            line: None,
        };
        *server
            .workspace
            .profiler_session
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(ProfilerSession::new(file_path, vec![hint]));
    }

    fn lens_params(uri: &Url) -> CodeLensParams {
        CodeLensParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        }
    }

    fn exec_params(cmd: &Command) -> ExecuteCommandParams {
        ExecuteCommandParams {
            command: cmd.command.clone(),
            arguments: cmd.arguments.clone().unwrap_or_default(),
            work_done_progress_params: Default::default(),
        }
    }

    #[test]
    fn lens_command_ids_are_all_supported() {
        for id in LENS_COMMAND_IDS {
            assert!(
                SUPPORTED_COMMANDS.contains(id),
                "lens command id {id:?} is not advertised or handled in SUPPORTED_COMMANDS"
            );
        }
    }

    #[tokio::test]
    async fn every_emitted_lens_is_dispatched() {
        let (service, uri) = build_server();
        let server = service.inner();
        add_profiler_session(server, &uri, "TestAlpha");

        let lenses = server
            .code_lens(lens_params(&uri))
            .await
            .expect("code_lens ok")
            .expect("expected lenses");
        assert!(!lenses.is_empty(), "fixture should emit lenses");

        // All three lens kinds must be represented so this exercises every arm.
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for lens in &lenses {
            let cmd = lens.command.as_ref().expect("lens carries a command");
            assert!(
                SUPPORTED_COMMANDS.contains(&cmd.command.as_str()),
                "emitted lens command {:?} is not handled",
                cmd.command
            );
            seen.insert(match cmd.command.as_str() {
                "al.findReferences" => "ref",
                "al.showProfiler" => "prof",
                "al.runTest" => "test",
                other => panic!("unexpected lens command {other}"),
            });
            // The dispatcher must return Some(..) for every emitted lens; the
            // unknown-command catch-all returns None.
            let result = server
                .execute_command(exec_params(cmd))
                .await
                .expect("execute_command ok");
            assert!(
                result.is_some(),
                "command {:?} fell through to the no-op catch-all",
                cmd.command
            );
        }
        assert!(seen.contains("ref"), "expected a reference lens");
        assert!(seen.contains("prof"), "expected a profiler lens");
        assert!(seen.contains("test"), "expected a test lens");
    }

    #[tokio::test]
    async fn find_references_command_returns_locations() {
        let (service, uri) = build_server();
        let server = service.inner();

        // Grab the reference lens for TestBeta (called once) and run its command.
        let lenses = server
            .code_lens(lens_params(&uri))
            .await
            .expect("ok")
            .expect("lenses");
        let ref_cmd = lenses
            .iter()
            .filter_map(|l| l.command.as_ref())
            .find(|c| c.command == "al.findReferences" && c.title.contains("1 reference"))
            .expect("a '1 reference' lens for TestBeta");

        let result = server
            .execute_command(exec_params(ref_cmd))
            .await
            .expect("ok")
            .expect("findReferences returns a value");
        let arr = result.as_array().expect("locations array");
        assert!(
            !arr.is_empty(),
            "expected at least one reference location, got {result:?}"
        );
        // Each entry must be a real LSP Location (has uri + range).
        assert!(arr[0].get("uri").is_some() && arr[0].get("range").is_some());
    }

    #[tokio::test]
    async fn show_profiler_command_returns_active_session() {
        let (service, uri) = build_server();
        let server = service.inner();
        add_profiler_session(server, &uri, "TestAlpha");

        let params = ExecuteCommandParams {
            command: "al.showProfiler".to_string(),
            arguments: vec![serde_json::json!({ "uri": uri })],
            work_done_progress_params: Default::default(),
        };
        let result = server
            .execute_command(params)
            .await
            .expect("ok")
            .expect("showProfiler returns a value");
        assert_eq!(result["active"], serde_json::json!(true));
        let hints = result["hints"].as_array().expect("hints array");
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0]["procedure"], serde_json::json!("TestAlpha"));
    }

    #[tokio::test]
    async fn run_test_without_server_reports_no_server() {
        let (service, _uri) = build_server();
        let server = service.inner();
        // No project / launch config loaded → routing must report `noServer`
        // (not a silent no-op) and never attempt a BC round-trip.
        let params = ExecuteCommandParams {
            command: "al.runTest".to_string(),
            arguments: vec![serde_json::json!({
                "codeunitId": 50200,
                "methodName": "TestAlpha",
            })],
            work_done_progress_params: Default::default(),
        };
        let result = server
            .execute_command(params)
            .await
            .expect("ok")
            .expect("runTest returns a value");
        assert_eq!(result["status"], serde_json::json!("noServer"));
        assert_eq!(
            result["target"]["methodName"],
            serde_json::json!("TestAlpha")
        );
    }

    #[tokio::test]
    async fn run_test_with_no_target_reports_invalid_args() {
        let (service, _uri) = build_server();
        let server = service.inner();
        let params = ExecuteCommandParams {
            command: "al.runTest".to_string(),
            arguments: vec![],
            work_done_progress_params: Default::default(),
        };
        let result = server
            .execute_command(params)
            .await
            .expect("ok")
            .expect("runTest returns a value");
        assert_eq!(result["status"], serde_json::json!("invalidArgs"));
    }

    #[tokio::test]
    async fn unknown_command_falls_through_to_none() {
        // Confirms the `Some(..)` assertions above are meaningful: a genuinely
        // unhandled command still hits the catch-all and returns None.
        let (service, _uri) = build_server();
        let server = service.inner();
        let params = ExecuteCommandParams {
            command: "al.thisDoesNotExist".to_string(),
            arguments: vec![],
            work_done_progress_params: Default::default(),
        };
        let result = server.execute_command(params).await.expect("ok");
        assert!(result.is_none(), "unknown command must return None");
    }
}

mod workspace_diagnostic_tests {
    //! `workspace/diagnostic` must report parse/syntax errors across the
    //! whole workspace — both background (never-opened) files and open documents
    //! — and report nothing for a clean workspace. Driven in-process against the
    //! real `AlServer` handler (no transport, no toolchain ⇒ bridge is a no-op,
    //! so these assert the syntax-pass aggregation).

    use super::*;

    const BAD_SRC: &str = "codeunit 50100 Test\n{\n    procedure Broken(\n    begin\n    end;\n}\n";
    const GOOD_SRC: &str =
        "codeunit 50100 MyCodeunit\n{\n    trigger OnRun()\n    begin\n    end;\n}\n";
    const GOOD_BG_SRC: &str =
        "codeunit 50101 OtherCodeunit\n{\n    trigger OnRun()\n    begin\n    end;\n}\n";

    fn new_ready_server() -> LspService<AlServer> {
        let (service, _socket) = LspService::new(AlServer::new);
        // Skip the 30s workspace-init wait in `await_ready`.
        service
            .inner()
            .workspace_init_state
            .send_replace(WorkspaceInitState::Ready);
        service
    }

    fn ws_diag_params() -> WorkspaceDiagnosticParams {
        WorkspaceDiagnosticParams {
            identifier: None,
            previous_result_ids: vec![],
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        }
    }

    /// Pull the per-file reports out of a workspace diagnostic result.
    fn reports(
        result: WorkspaceDiagnosticReportResult,
    ) -> Vec<WorkspaceFullDocumentDiagnosticReport> {
        match result {
            WorkspaceDiagnosticReportResult::Report(r) => r
                .items
                .into_iter()
                .map(|item| match item {
                    WorkspaceDocumentDiagnosticReport::Full(f) => f,
                    WorkspaceDocumentDiagnosticReport::Unchanged(_) => {
                        panic!("did not expect an Unchanged report")
                    }
                })
                .collect(),
            WorkspaceDiagnosticReportResult::Partial(_) => {
                panic!("did not expect a Partial result")
            }
        }
    }

    #[tokio::test]
    async fn reports_background_file_syntax_error() {
        // A file present only in the FileIndex (never opened) with a syntax error
        // must be reported via the workspace-scope path.
        let service = new_ready_server();
        let server = service.inner();
        let uri = Url::parse("file:///proj/Bad.al").expect("valid uri");
        // Mirror an indexed-but-unopened file: on_document_change parses + indexes
        // without inserting into the open-document set.
        al_workspace::on_document_change(&server.workspace, &uri, BAD_SRC);

        let result = server
            .workspace_diagnostic(ws_diag_params())
            .await
            .expect("workspace_diagnostic succeeds");
        let files = reports(result);
        let report = files
            .iter()
            .find(|f| f.uri == uri)
            .expect("the bad background file should be reported");
        assert!(
            !report.full_document_diagnostic_report.items.is_empty(),
            "expected at least one diagnostic for the bad file"
        );
        assert_eq!(
            report.version, None,
            "an unopened workspace file has no document version"
        );
    }

    #[tokio::test]
    async fn reports_open_document_syntax_error() {
        let service = new_ready_server();
        let server = service.inner();
        let uri = Url::parse("file:///proj/OpenBad.al").expect("valid uri");
        server
            .workspace
            .documents
            .open(uri.clone(), BAD_SRC.to_string())
            .unwrap();
        al_workspace::on_document_change(&server.workspace, &uri, BAD_SRC);

        let result = server
            .workspace_diagnostic(ws_diag_params())
            .await
            .expect("workspace_diagnostic succeeds");
        let files = reports(result);
        // The open document must appear exactly once (no double-report from the
        // file-index pass).
        let matches: Vec<_> = files.iter().filter(|f| f.uri == uri).collect();
        assert_eq!(
            matches.len(),
            1,
            "open document should be reported exactly once, got {}",
            matches.len()
        );
        assert!(
            !matches[0].full_document_diagnostic_report.items.is_empty(),
            "expected diagnostics for the open bad document"
        );
    }

    #[tokio::test]
    async fn clean_workspace_reports_none() {
        let service = new_ready_server();
        let server = service.inner();
        let open_uri = Url::parse("file:///proj/OpenGood.al").expect("valid uri");
        server
            .workspace
            .documents
            .open(open_uri.clone(), GOOD_SRC.to_string())
            .unwrap();
        al_workspace::on_document_change(&server.workspace, &open_uri, GOOD_SRC);
        // A second clean file that is only indexed, never opened.
        let bg_uri = Url::parse("file:///proj/BgGood.al").expect("valid uri");
        al_workspace::on_document_change(&server.workspace, &bg_uri, GOOD_BG_SRC);

        let result = server
            .workspace_diagnostic(ws_diag_params())
            .await
            .expect("workspace_diagnostic succeeds");
        let files = reports(result);
        assert!(
            files.is_empty(),
            "clean workspace must report no files, got: {:?}",
            files.iter().map(|f| f.uri.as_str()).collect::<Vec<_>>()
        );
    }
}

mod project_diagnostics_convergence_tests {
    use super::*;

    /// The project-scope push pass staged under the generation read guard and
    /// restarted whenever `generation_revision` moved, which every keystroke
    /// does. On a project where the pass outlasts the typing gaps it never
    /// published. The retry is now bounded, so continuous edits still end in a
    /// publication.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn project_diagnostics_publish_while_the_workspace_keeps_changing() {
        let (service, _socket) = LspService::new(AlServer::new);
        let server = service.inner();
        server
            .workspace_init_state
            .send_replace(WorkspaceInitState::Ready);

        let uri = Url::parse("file:///proj/Foo.Codeunit.al").unwrap();
        server
            .workspace
            .documents
            .open(uri.clone(), "codeunit 50100 Foo\n{\n}\n".to_string())
            .unwrap();

        let churn_workspace = Arc::clone(&server.workspace);
        let churn_uri = uri.clone();
        let churn = tokio::spawn(async move {
            loop {
                // Mirror `did_change`: the edit takes the generation write
                // guard, so the pass never sees a half-applied mutation.
                let generation = churn_workspace.generation_lock.write().await;
                if let Some(text) = churn_workspace.documents.get_text(&churn_uri) {
                    al_workspace::on_document_change(&churn_workspace, &churn_uri, &text);
                }
                drop(generation);
                tokio::task::yield_now().await;
            }
        });

        let published = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            diagnostics::publish_workspace_diagnostics_parts(
                Arc::clone(&server.workspace),
                server.client.clone(),
                Arc::clone(&server.semantic_diagnostic_cache),
                Arc::clone(&server.workspace_diagnostic_uris),
                None,
                &server.session,
            ),
        )
        .await;
        churn.abort();
        assert!(
            published.expect("the staging loop must converge, not retry forever"),
            "the pass must publish rather than give up silently"
        );
    }
}

mod did_change_offload_tests {
    use super::*;

    fn large_codeunit(procedures: usize) -> String {
        let mut text = String::from("codeunit 50100 Big\n{\n");
        for i in 0..procedures {
            text.push_str(&format!(
                "    procedure P{i}(Value: Integer): Integer\n    begin\n        exit(Value + {i});\n    end;\n"
            ));
        }
        text.push_str("}\n");
        text
    }

    /// The reparse and re-index that `did_change` performs now runs on the
    /// blocking pool. It still happens under the same generation write guard,
    /// so the document store and the file index must never be observable out
    /// of step: after the call returns, both reflect the new text.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_offloaded_reindex_leaves_the_store_and_the_index_in_step() {
        let (service, _socket) = LspService::new(AlServer::new);
        let server = service.inner();
        server
            .workspace_init_state
            .send_replace(WorkspaceInitState::Ready);

        let uri = Url::parse("file:///proj/Big.Codeunit.al").unwrap();
        let path = std::path::Path::new("/proj/Big.Codeunit.al");
        server
            .workspace
            .documents
            .open_with_client_version(uri.clone(), large_codeunit(200), 1)
            .unwrap();
        al_workspace::on_document_change(
            &server.workspace,
            &uri,
            &server.workspace.documents.get_text(&uri).unwrap(),
        );

        server
            .did_change(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: uri.clone(),
                    version: 2,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: large_codeunit(201),
                }],
            })
            .await;

        let (text, version) = server
            .workspace
            .documents
            .get_text_and_client_version(&uri)
            .expect("document stays open");
        assert_eq!(version, 2, "the edit must have been applied");
        assert!(text.contains("P200"), "the store holds the new text");
        assert!(
            server
                .workspace
                .file_index
                .procedures_snapshot(path)
                .iter()
                .any(|name| name.eq_ignore_ascii_case("P200")),
            "the file index must reflect the same edit by the time did_change returns"
        );
    }
}

mod lock_order_tests {
    use super::*;

    /// Stand in for `did_change_configuration` publishing a symbol generation:
    /// it holds the project write guard and then waits for the config write
    /// guard. Returns whether the config guard was granted.
    async fn publish_configuration(
        server: &AlServer,
        project: tokio::sync::RwLockWriteGuard<'_, Option<al_project::project::AlProject>>,
    ) -> bool {
        // Let the diagnostics pass reach its own lock waits first.
        tokio::task::yield_now().await;
        let config = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            server.workspace.config.write(),
        )
        .await;
        let granted = config.is_ok();
        drop(config);
        drop(project);
        granted
    }

    /// The pull path's syntax pass took the config read guard and then waited
    /// for the project read guard. A configuration publish holds the project
    /// and waits for the config, so each waited on the other for good.
    #[tokio::test]
    async fn pull_diagnostics_do_not_hold_the_config_while_waiting_for_the_project() {
        let (service, _socket) = LspService::new(AlServer::new);
        let server = service.inner();
        server.workspace.config.write().await.enable_code_analysis = false;
        let uri = Url::parse("file:///proj/Foo.Codeunit.al").unwrap();

        let project = server.workspace.project.write().await;
        let (_, granted) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(
                diagnostics::compute_diagnostics(server, &uri, ""),
                publish_configuration(server, project),
            )
        })
        .await
        .expect("the diagnostics pass finishes once the publisher lets go");

        assert!(
            granted,
            "a configuration publish must not wait on a diagnostics pass that waits on it"
        );
    }

    /// The push path shares the syntax pass, and with it the same lock order.
    #[tokio::test]
    async fn push_diagnostics_do_not_hold_the_config_while_waiting_for_the_project() {
        let (service, _socket) = LspService::new(AlServer::new);
        let server = service.inner();
        server.workspace.config.write().await.enable_code_analysis = false;
        let uri = Url::parse("file:///proj/Foo.Codeunit.al").unwrap();
        server
            .workspace
            .documents
            .open_with_client_version(uri.clone(), "codeunit 50100 Foo\n{\n}\n".to_string(), 1)
            .unwrap();
        let (text, version) = server
            .workspace
            .documents
            .get_text_and_client_version(&uri)
            .expect("the document is open");

        let project = server.workspace.project.write().await;
        let ((), granted) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(
                diagnostics::publish_diagnostics(server, &uri, text, version),
                publish_configuration(server, project),
            )
        })
        .await
        .expect("the diagnostics pass finishes once the publisher lets go");

        assert!(
            granted,
            "a configuration publish must not wait on a diagnostics pass that waits on it"
        );
    }
}
