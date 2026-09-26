use super::*;

mod runtime_dir_tests {
    use super::ensure_private_dir;

    #[test]
    fn accepts_a_directory_this_user_owns() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("al-lsp");
        ensure_private_dir(&dir).unwrap();
        assert!(dir.is_dir());
    }

    #[test]
    fn re_asserts_owner_only_on_a_directory_left_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("al-lsp");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        ensure_private_dir(&dir).unwrap();

        let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{mode:o}");
    }

    #[test]
    fn refuses_a_runtime_directory_that_is_a_symbolic_link() {
        let tmp = tempfile::tempdir().unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        let link = tmp.path().join("al-lsp");
        std::os::unix::fs::symlink(&elsewhere, &link).unwrap();

        let error = ensure_private_dir(&link).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("symbolic link"), "{error}");
    }

    /// macOS: `TMPDIR` is under `/var`, a root-owned link to `/private/var`.
    /// A link this user or root owns, in a directory that passed, can only be
    /// changed by them, so the runtime directory beneath it is accepted once
    /// the directories it resolves to pass too.
    #[test]
    fn accepts_a_runtime_directory_under_a_link_this_user_owns() {
        let tmp = tempfile::tempdir().unwrap();
        let private = tmp.path().join("private");
        std::fs::create_dir(&private).unwrap();
        let var = tmp.path().join("var");
        std::os::unix::fs::symlink(&private, &var).unwrap();

        ensure_private_dir(&var.join("al-lsp")).unwrap();

        assert!(private.join("al-lsp").is_dir());
    }

    /// The finding's case: on a shared host with `XDG_RUNTIME_DIR` unset the
    /// runtime path is `/tmp/<victim>/al-lsp`, and a local attacker who owns
    /// `/tmp/<victim>` can replace the `al-lsp` entry whatever its own mode is.
    /// Ownership of `/tmp` itself is fine, because it is root-owned and sticky.
    #[test]
    fn a_root_owned_world_writable_parent_is_only_accepted_when_sticky() {
        // macOS `/tmp` is a root-owned link to `/private/tmp`. The owner check
        // is for a directory, and a link at that position is refused on
        // purpose, so ask about the directory the link names.
        let shared = std::fs::canonicalize("/tmp").unwrap();
        let shared = shared.as_path();
        let metadata = std::fs::metadata(shared).unwrap();
        use std::os::unix::fs::MetadataExt;
        assert_eq!(metadata.uid(), 0, "/tmp is expected to be root-owned");
        assert_ne!(metadata.mode() & 0o1000, 0, "/tmp is expected to be sticky");
        al_protocol::endpoint::check_directory_owner(shared).unwrap();
    }
}

mod dispatch_tests {
    use super::{
        dispatch_diag, dispatch_request, ensure_document, extract_i32, extract_position,
        extract_uri, file_not_found, file_uri_from_params, invalid_params, parse_object_kind,
        read_bounded_line, require_document_text, require_project_root, resolve_idle_timeout,
        rpc_error, set_test_project_root, CredentialUse, PathUse, DEFAULT_IDLE_TIMEOUT,
        DISPATCHERS, IDLE_TIMEOUT_ENV,
    };
    use al_protocol::jsonrpc::{error_codes, Request};
    use futures::FutureExt;
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};
    use std::time::Duration;
    use tokio::sync::Notify;

    /// Serialises the tests that set `AL_DAEMON_IDLE_SECS`.
    static IDLE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_idle_env<T>(value: Option<&str>, body: impl FnOnce() -> T) -> T {
        let _guard = IDLE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var_os(IDLE_TIMEOUT_ENV);
        match value {
            Some(value) => std::env::set_var(IDLE_TIMEOUT_ENV, value),
            None => std::env::remove_var(IDLE_TIMEOUT_ENV),
        }
        let result = body();
        match saved {
            Some(value) => std::env::set_var(IDLE_TIMEOUT_ENV, value),
            None => std::env::remove_var(IDLE_TIMEOUT_ENV),
        }
        result
    }

    #[test]
    fn idle_timeout_defaults_to_thirty_minutes() {
        let resolved = with_idle_env(None, || resolve_idle_timeout(None));
        assert_eq!(resolved, Some(DEFAULT_IDLE_TIMEOUT));
        assert_eq!(DEFAULT_IDLE_TIMEOUT, Duration::from_secs(30 * 60));
    }

    #[test]
    fn the_command_line_beats_the_environment() {
        let resolved = with_idle_env(Some("900"), || {
            resolve_idle_timeout(Some(Duration::from_secs(5)))
        });
        assert_eq!(resolved, Some(Duration::from_secs(5)));
    }

    #[test]
    fn the_environment_sets_a_short_timeout() {
        let resolved = with_idle_env(Some("2"), || resolve_idle_timeout(None));
        assert_eq!(resolved, Some(Duration::from_secs(2)));
    }

    /// Zero is the way to keep a daemon that an editor session owns.
    #[test]
    fn zero_disables_the_idle_exit() {
        assert_eq!(
            with_idle_env(Some("0"), || resolve_idle_timeout(None)),
            None
        );
        assert_eq!(resolve_idle_timeout(Some(Duration::ZERO)), None);
    }

    #[test]
    fn an_unparsable_timeout_falls_back_to_the_default() {
        let resolved = with_idle_env(Some("half an hour"), || resolve_idle_timeout(None));
        assert_eq!(resolved, Some(DEFAULT_IDLE_TIMEOUT));
    }

    /// A `ping` pipelined behind a long call on the same connection used to
    /// wait for it, because the loop awaited each dispatch before reading the
    /// next line. The debug-session mutex gives a dispatch the test can hold
    /// open for as long as it likes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_pipelined_request_is_answered_while_a_long_one_runs() {
        use super::handle_connection;
        use interprocess::local_socket::tokio::prelude::*;
        use interprocess::local_socket::{GenericFilePath, ListenerOptions, ToFsName};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("daemon.sock");
        let name = endpoint
            .as_path()
            .to_fs_name::<GenericFilePath>()
            .expect("socket name");
        let listener = ListenerOptions::new()
            .name(name)
            .create_tokio()
            .expect("listener");

        let workspace = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = std::sync::Arc::new(Notify::new());
        let server = {
            let workspace = std::sync::Arc::clone(&workspace);
            tokio::spawn(async move {
                let stream = listener.accept().await.expect("accept");
                let _ = handle_connection(
                    stream,
                    workspace,
                    std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
                    std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                    shutdown,
                )
                .await;
            })
        };

        let name = endpoint
            .as_path()
            .to_fs_name::<GenericFilePath>()
            .expect("socket name");
        let client = interprocess::local_socket::tokio::Stream::connect(name)
            .await
            .expect("connect");
        let (reader, mut writer) = client.split();
        let mut reader = tokio::io::BufReader::new(reader);

        // `debug {"cmd":"state"}` waits on the debug-session mutex, which the
        // test holds, so its dispatch cannot finish yet.
        let blocked = workspace.debug_session.lock().await;
        let batch = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"debug","params":{"cmd":"state"}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":99,"method":"ping"}"#,
            "\n",
        );
        writer.write_all(batch.as_bytes()).await.expect("write");
        writer.flush().await.expect("flush");

        let mut line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            reader.read_line(&mut line),
        )
        .await
        .expect("a pipelined ping must not wait for the request ahead of it")
        .expect("read");
        let frame: serde_json::Value = serde_json::from_str(line.trim()).expect("json");
        assert_eq!(frame["id"], 99, "the ping must be answered first: {frame}");
        assert_eq!(frame["result"], "pong");

        // Releasing the mutex lets the queued request finish and answer too.
        drop(blocked);
        let mut line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            reader.read_line(&mut line),
        )
        .await
        .expect("the blocked request must still be answered")
        .expect("read");
        let frame: serde_json::Value = serde_json::from_str(line.trim()).expect("json");
        assert_eq!(frame["id"], 1);

        drop(writer);
        drop(reader);
        let _ = server.await;
    }

    fn dispatched_method_literals() -> BTreeSet<String> {
        DISPATCHERS
            .iter()
            .map(|dispatcher| dispatcher.method.to_string())
            .collect()
    }

    /// The method names the reference lists as a catalogue: a line that holds
    /// backticked names, separators and nothing else, with an optional label
    /// such as `Tests:`. Prose lines that mention a method in passing, and the
    /// `debug` subcommand list, do not match.
    fn documented_method_catalog(reference: &str) -> BTreeSet<String> {
        let mut methods = BTreeSet::new();
        for line in reference.lines() {
            // A heading names its subject, not the catalogue: "## Projection:
            // `limit`, `offset`, `fields`" is three parameters.
            if line.starts_with('#') {
                continue;
            }
            let body = match line.split_once(": ") {
                Some((label, rest)) if !label.contains('`') => rest,
                _ => line,
            };
            let outside_ticks = body
                .split('`')
                .step_by(2)
                .all(|text| text.chars().all(|c| matches!(c, ',' | '.' | ' ')));
            let names = body.split('`').skip(1).step_by(2).collect::<Vec<_>>();
            if !outside_ticks || names.is_empty() || body.matches('`').count() % 2 != 0 {
                continue;
            }
            methods.extend(names.into_iter().map(str::to_string));
        }
        methods
    }

    /// The daemon reference and MCP's generic `al_call` promise the complete
    /// dispatcher, not a hand-picked subset. Keep the human reference pinned
    /// directly to the dispatch table so newly registered methods cannot become
    /// undocumented agent-only knowledge.
    #[test]
    fn daemon_reference_names_every_dispatched_method() {
        let methods = dispatched_method_literals();
        assert!(
            methods.len() >= 80,
            "the dispatch table unexpectedly holds only {} methods",
            methods.len()
        );
        let reference = include_str!("../../../../../Docs/reference/daemon-methods.md");
        let missing = methods
            .iter()
            .filter(|method| !reference.contains(&format!("`{method}`")))
            .cloned()
            .collect::<Vec<_>>();
        assert!(
            missing.is_empty(),
            "Docs/reference/daemon-methods.md omits dispatched methods: {missing:?}"
        );
    }

    /// The other direction, which the dispatcher-to-docs check cannot see: a
    /// method that stopped dispatching, or was renamed, stays in the catalogue
    /// and a caller follows the reference into `METHOD_NOT_FOUND`.
    #[test]
    fn the_reference_catalogue_lists_only_methods_that_dispatch() {
        let reference = include_str!("../../../../../Docs/reference/daemon-methods.md");
        let catalogue = documented_method_catalog(reference);
        assert!(
            catalogue.len() >= 80,
            "the catalogue parser found only {} names: {catalogue:?}",
            catalogue.len()
        );
        let dispatched = dispatched_method_literals();
        let stale = catalogue
            .difference(&dispatched)
            .cloned()
            .collect::<Vec<_>>();
        assert!(
            stale.is_empty(),
            "Docs/reference/daemon-methods.md lists methods the daemon does not dispatch: {stale:?}"
        );
    }

    /// Every method that opens or rewrites the file its caller names refuses a
    /// path outside the project, driven through the dispatcher rather than
    /// through the helper the dispatcher is supposed to call. `rename` passed
    /// the helper's own tests while skipping the helper.
    #[tokio::test]
    async fn every_path_dispatcher_refuses_a_file_outside_the_project() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let (workspace, _) = project_with_doc(&root);
        let workspace = std::sync::Arc::new(workspace);
        let outside = dir.path().join("id_rsa");
        std::fs::write(&outside, b"PRIVATE KEY").unwrap();
        let shutdown = Notify::new();

        let mut checked = 0;
        for dispatcher in DISPATCHERS {
            if dispatcher.path == PathUse::None {
                continue;
            }
            checked += 1;
            // Every parameter any path method needs beside the path itself, so
            // the request reaches the containment check rather than stopping at
            // a missing argument.
            let params = serde_json::json!({
                "file": outside.to_str().unwrap(),
                "line": 0,
                "character": 0,
                "newName": "Renamed",
                "rule": "AL-NL002",
            });
            let response = dispatch_request(
                &workspace,
                Request::new(1, dispatcher.method, Some(params)),
                &shutdown,
            )
            .await;
            let error = response.error.unwrap_or_else(|| {
                panic!(
                    "{} answered for a path outside the project",
                    dispatcher.method
                )
            });
            assert_eq!(
                error.code,
                error_codes::PATH_NOT_AUTHORIZED,
                "{} must refuse an outside path with the code the CLI retries on: {error}",
                dispatcher.method
            );
            assert!(
                !workspace
                    .documents
                    .contains(&url::Url::from_file_path(&outside).unwrap()),
                "{} left the refused file in the document store",
                dispatcher.method
            );
        }
        assert!(
            checked >= 15,
            "only {checked} dispatchers declare a path parameter"
        );
    }

    /// The daemon and `al-explorer` agree on which methods take `text` because
    /// they read the same list. The dispatch table is the other half: a method
    /// declared `Read` accepts `text`, and the CLI resends the file's text on a
    /// refusal for exactly those.
    #[test]
    fn the_text_capable_methods_are_the_read_dispatchers() {
        let declared = DISPATCHERS
            .iter()
            .filter(|dispatcher| dispatcher.path == PathUse::Read)
            .map(|dispatcher| dispatcher.method)
            .collect::<BTreeSet<_>>();
        let shared = al_protocol::methods::TEXT_CAPABLE_METHODS
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            declared, shared,
            "al_protocol::methods::TEXT_CAPABLE_METHODS and the read dispatchers have drifted"
        );
    }

    /// A project whose launch file names an on-premises server nobody has
    /// trusted, which is what a clone gives an attacker.
    fn untrusted_project_with_a_launch_file(root: &Path) -> al_workspace::Workspace {
        std::fs::create_dir_all(root.join(".vscode")).unwrap();
        std::fs::write(
            root.join("app.json"),
            serde_json::json!({
                "id": "00000000-0000-0000-0000-000000000001",
                "name": "Test",
                "publisher": "Test",
                "version": "1.0.0.0",
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            root.join(".vscode/launch.json"),
            serde_json::json!({
                "configurations": [{
                    "name": "Local",
                    "type": "al",
                    "request": "launch",
                    "environmentType": "OnPrem",
                    "server": "https://collector.example.test",
                    "serverInstance": "BC",
                    "authentication": "AAD",
                    "tenant": "tenant-id",
                }]
            })
            .to_string(),
        )
        .unwrap();
        let workspace = al_workspace::Workspace::new();
        set_test_project_root(&workspace, root);
        workspace
    }

    /// Every method declared as reaching a Business Central credential refuses
    /// the server an untrusted repository names.
    ///
    /// `tests.snapshot_capture` and `tests.snapshot_replay` reach the same
    /// decision through `debug_dispatch::acquire_bc_token`, whose own tests
    /// drive it directly: getting them this far needs an indexed test codeunit
    /// and a live breakpoint set, which says nothing more about the gate.
    #[tokio::test]
    async fn an_authorized_dispatcher_refuses_an_untrusted_repository_target() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let workspace = std::sync::Arc::new(untrusted_project_with_a_launch_file(&root));
        let shutdown = Notify::new();

        for (method, params) in [
            (
                "debug",
                serde_json::json!({ "cmd": "start", "config": "Local" }),
            ),
            ("publish", serde_json::json!({ "config": "Local" })),
        ] {
            let response =
                dispatch_request(&workspace, Request::new(1, method, Some(params)), &shutdown)
                    .await;
            let text = serde_json::to_string(&response).unwrap();
            assert!(
                text.contains("not trusted"),
                "{method} must refuse the repository's server: {text}"
            );
        }
    }

    /// Which methods reach a Business Central credential is a decision, not a
    /// detail: adding one to the dispatch table has to be deliberate, and the
    /// trust documentation names the same set.
    #[test]
    fn the_authorized_dispatchers_are_the_ones_the_trust_doc_names() {
        let declared = DISPATCHERS
            .iter()
            .filter(|dispatcher| dispatcher.credential == CredentialUse::Authorized)
            .map(|dispatcher| dispatcher.method)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            declared,
            BTreeSet::from([
                "debug",
                "downloadSymbols",
                "publish",
                "tests.snapshot_capture",
                "tests.snapshot_replay",
            ]),
            "a method that spends a Business Central credential was added or removed"
        );

        // The trust documentation is where a user reads which methods those
        // are, so it names each one.
        let trust_doc = include_str!("../../../../../Docs/features/project-trust.md");
        let credentials = trust_doc
            .split_once("## Credentials")
            .expect("the trust doc has a Credentials section")
            .1;
        for method in &declared {
            assert!(
                credentials.contains(&format!("`{method}`")),
                "Docs/features/project-trust.md does not name `{method}` under Credentials"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn ensure_private_dir_creates_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("al-lsp");
        super::ensure_private_dir(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "newly created dir must be 0o700");
    }

    #[cfg(unix)]
    #[test]
    fn ensure_private_dir_tightens_preexisting_lax_dir() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("al-lsp");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        super::ensure_private_dir(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o700,
            "pre-existing 0o755 dir must be tightened to 0o700"
        );
    }

    #[test]
    fn extract_i32_accepts_in_range() {
        let params = serde_json::json!({ "id": 50_100 });
        assert_eq!(extract_i32(&params, "id"), Some(50_100));
        let params = serde_json::json!({ "id": -1 });
        assert_eq!(extract_i32(&params, "id"), Some(-1));
        let params = serde_json::json!({ "id": i32::MAX });
        assert_eq!(extract_i32(&params, "id"), Some(i32::MAX));
        let params = serde_json::json!({ "id": i32::MIN });
        assert_eq!(extract_i32(&params, "id"), Some(i32::MIN));
    }

    #[test]
    fn extract_i32_rejects_overflow() {
        let params = serde_json::json!({ "id": (i32::MAX as i64) + 1 });
        assert_eq!(extract_i32(&params, "id"), None);
        let params = serde_json::json!({ "id": (i32::MIN as i64) - 1 });
        assert_eq!(extract_i32(&params, "id"), None);
        let params = serde_json::json!({ "id": u64::MAX });
        assert_eq!(extract_i32(&params, "id"), None);
    }

    #[test]
    fn extract_i32_rejects_missing_or_wrong_type() {
        let params = serde_json::json!({});
        assert_eq!(extract_i32(&params, "id"), None);
        let params = serde_json::json!({ "id": "fifty" });
        assert_eq!(extract_i32(&params, "id"), None);
        let params = serde_json::json!({ "id": 3.5 });
        assert_eq!(extract_i32(&params, "id"), None);
    }

    #[test]
    fn extract_position_rejects_overflow() {
        let params = serde_json::json!({ "line": (u32::MAX as u64) + 1, "character": 0 });
        assert!(extract_position(&params).is_none());
    }

    #[test]
    fn socket_path_is_deterministic() {
        std::env::set_var("XDG_RUNTIME_DIR", "/tmp");
        let p = std::path::Path::new("/tmp");
        let path1 = al_protocol::socket_path(p)
            .expect("socket_path returned None with XDG_RUNTIME_DIR set");
        let path2 = al_protocol::socket_path(p)
            .expect("socket_path returned None with XDG_RUNTIME_DIR set");
        assert_eq!(path1, path2);
        assert!(path1.to_str().unwrap().ends_with(".sock"));
        let filename = path1.file_name().unwrap().to_str().unwrap();
        let hash_part = filename.strip_suffix(".sock").unwrap();
        assert_eq!(hash_part.len(), 16);
        assert!(hash_part.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn bounded_read_accepts_line_within_limit() {
        let input = b"hello world\n";
        let mut reader = tokio::io::BufReader::new(input.as_ref());
        let result = read_bounded_line(&mut reader, 64).await.unwrap();
        assert_eq!(result, Some("hello world".to_string()));
    }

    #[tokio::test]
    async fn bounded_read_rejects_line_exceeding_limit() {
        let input = b"0123456789";
        let mut reader = tokio::io::BufReader::new(input.as_ref());
        let err = read_bounded_line(&mut reader, 5).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("byte limit"));
    }

    #[tokio::test]
    async fn bounded_read_returns_none_on_empty_eof() {
        let input: &[u8] = b"";
        let mut reader = tokio::io::BufReader::new(input);
        let result = read_bounded_line(&mut reader, 64).await.unwrap();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn bounded_read_rejects_line_with_newline_exceeding_limit() {
        let input = b"0123456789\nmore data";
        let mut reader = tokio::io::BufReader::new(input.as_ref());
        let err = read_bounded_line(&mut reader, 5).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("byte limit"));
    }

    #[tokio::test]
    async fn bounded_read_returns_partial_line_on_eof() {
        let input = b"no newline here";
        let mut reader = tokio::io::BufReader::new(input.as_ref());
        let result = read_bounded_line(&mut reader, 64).await.unwrap();
        assert_eq!(result, Some("no newline here".to_string()));
    }

    /// A workspace whose project root is `dir`, plus a `doc.al` inside it.
    fn project_with_doc(dir: &Path) -> (al_workspace::Workspace, PathBuf) {
        let file = dir.join("doc.al");
        std::fs::write(&file, b"x").unwrap();
        let workspace = al_workspace::Workspace::new();
        set_test_project_root(&workspace, dir);
        (workspace, file)
    }

    #[test]
    fn file_uri_accepts_existing_local_uri() {
        let dir = tempfile::tempdir().unwrap();
        let (workspace, file) = project_with_doc(dir.path());
        let params = serde_json::json!({
            "uri": url::Url::from_file_path(&file).unwrap(),
        });
        let uri = file_uri_from_params(&workspace, &params)
            .expect("uri must be valid")
            .expect("uri must be present");
        assert_eq!(uri.to_file_path().unwrap(), file.canonicalize().unwrap());
    }

    #[test]
    fn file_uri_canonicalizes_absolute_existing_path() {
        let dir = tempfile::tempdir().unwrap();
        let (workspace, file) = project_with_doc(dir.path());
        let params = serde_json::json!({ "file": file.to_str().unwrap() });
        let uri = file_uri_from_params(&workspace, &params)
            .expect("absolute file path must be valid")
            .expect("absolute file path must produce a uri");
        let canon = file.canonicalize().unwrap();
        assert_eq!(uri.to_file_path().unwrap(), canon);
    }

    #[test]
    fn file_uri_resolves_relative_path_against_the_project_root() {
        let dir = tempfile::tempdir().unwrap();
        let (workspace, file) = project_with_doc(dir.path());
        let params = serde_json::json!({ "file": "doc.al" });
        let uri = file_uri_from_params(&workspace, &params)
            .expect("relative path must be valid")
            .expect("relative path must produce a uri");
        assert_eq!(uri.to_file_path().unwrap(), file.canonicalize().unwrap());
    }

    #[test]
    fn file_uri_rejects_a_path_outside_the_project() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let secret = outside.join("id_rsa");
        std::fs::write(&secret, b"PRIVATE KEY").unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let (workspace, _) = project_with_doc(&root);

        for params in [
            serde_json::json!({ "file": secret.to_str().unwrap() }),
            serde_json::json!({ "file": "../outside/id_rsa" }),
            serde_json::json!({ "uri": url::Url::from_file_path(&secret).unwrap() }),
        ] {
            let error = file_uri_from_params(&workspace, &params)
                .expect_err("a path outside the project must be rejected");
            assert!(
                error.message.contains("outside the project"),
                "{error}: {params}"
            );
            assert_eq!(
                error.code,
                error_codes::PATH_NOT_AUTHORIZED,
                "a refused path needs the code the CLI retries on: {params}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn file_uri_rejects_a_symlink_that_escapes_the_project() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("id_rsa"), b"PRIVATE KEY").unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        let (workspace, _) = project_with_doc(&root);

        let params = serde_json::json!({ "file": "link/id_rsa" });
        let error = file_uri_from_params(&workspace, &params)
            .expect_err("a symlink out of the project must be rejected");
        assert!(error.message.contains("outside the project"), "{error}");
    }

    #[test]
    fn file_uri_rejects_every_path_when_no_project_is_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("doc.al");
        std::fs::write(&file, b"x").unwrap();
        let workspace = al_workspace::Workspace::new();
        let params = serde_json::json!({ "file": file.to_str().unwrap() });
        let error = file_uri_from_params(&workspace, &params)
            .expect_err("without a project there is nothing to contain against");
        assert!(error.message.contains("No project is loaded"), "{error}");
    }

    #[test]
    fn file_uri_rejects_missing_path_instead_of_falling_back() {
        let dir = tempfile::tempdir().unwrap();
        let (workspace, _) = project_with_doc(dir.path());
        let params = serde_json::json!({ "file": "not/existing/al-test-xyz.al" });
        let error = file_uri_from_params(&workspace, &params)
            .expect_err("nonexistent path must be rejected");
        assert!(
            error.message.contains("is not a regular file"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn file_uri_returns_absent_without_uri_or_file() {
        let dir = tempfile::tempdir().unwrap();
        let (workspace, _) = project_with_doc(dir.path());
        let params = serde_json::json!({ "something": "else" });
        assert_eq!(
            file_uri_from_params(&workspace, &params).expect("no path is not an error"),
            None
        );
    }

    #[test]
    fn file_uri_rejects_ambiguous_or_malformed_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let (workspace, _) = project_with_doc(dir.path());
        for params in [
            serde_json::json!({"uri": "file:///tmp/x.al", "file": "/tmp/x.al"}),
            serde_json::json!({"uri": 7}),
            serde_json::json!({"uri": "not a url"}),
            serde_json::json!({"uri": "https://example.com/Test.al"}),
            serde_json::json!({"file": false}),
            serde_json::json!({"file": "  "}),
        ] {
            assert!(
                file_uri_from_params(&workspace, &params).is_err(),
                "malformed input must be rejected: {params}"
            );
        }
    }

    /// A read-only method reaches a file outside the project only through the
    /// text its caller supplies. Without that text the path is refused, and
    /// with the dedicated code, which is what tells `al-explorer` to read the
    /// file and ask again.
    #[test]
    fn a_read_refuses_an_outside_path_it_was_given_no_text_for() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let file = outside.join("ErrorCases.al");
        std::fs::write(&file, b"codeunit 1 Broken { procedure").unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let (workspace, _) = project_with_doc(&root);

        let params = serde_json::json!({ "file": file.to_str().unwrap() });
        let response = super::build_dispatch::dispatch_parse(&workspace, 1, &params);
        let error = response.error.expect("an outside path must be refused");
        assert_eq!(error.code, error_codes::PATH_NOT_AUTHORIZED, "{error}");
        assert!(error.message.contains("outside the project"), "{error}");
    }

    /// With the text, the same read is answered from what the caller sent, the
    /// path is never opened, and nothing is left behind in the document store
    /// for a later workspace-wide query to pick up.
    #[test]
    fn a_read_answers_an_outside_path_from_the_supplied_text() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let (workspace, _) = project_with_doc(&root);
        // Never written to disk: the daemon must answer without opening it.
        let absent = dir.path().join("outside").join("ErrorCases.al");

        let params = serde_json::json!({
            "file": absent.to_str().unwrap(),
            "text": "codeunit 50100 Broken\n{\n    procedure\n}\n",
        });
        let response = super::build_dispatch::dispatch_parse(&workspace, 2, &params);
        assert!(response.error.is_none(), "{:?}", response.error);
        let errors = response
            .result
            .as_ref()
            .and_then(|result| result.get("errors"))
            .and_then(serde_json::Value::as_u64)
            .expect("parse reports an error count");
        assert!(errors > 0, "the supplied text does not parse cleanly");

        let uri = url::Url::from_file_path(&absent).unwrap();
        assert!(
            !workspace.documents.contains(&uri),
            "a supplied document must not outlive the request that needed it"
        );
    }

    /// Text for a path *inside* the project is refused: the daemon holds that
    /// file itself, and substituting content for it would let a later write
    /// flush a caller's text over the real source.
    #[test]
    fn supplied_text_is_refused_for_a_path_inside_the_project() {
        let dir = tempfile::tempdir().unwrap();
        let (workspace, file) = project_with_doc(dir.path());
        let params = serde_json::json!({
            "file": file.to_str().unwrap(),
            "text": "codeunit 50100 Substituted { }",
        });
        let error = super::build_dispatch::dispatch_parse(&workspace, 3, &params)
            .error
            .expect("text for a project file must be refused");
        assert_eq!(error.code, error_codes::INVALID_PARAMS, "{error}");
        assert!(error.message.contains("inside the project"), "{error}");
    }

    /// A method that rewrites the file it names takes no text at all.
    #[tokio::test]
    async fn a_write_refuses_supplied_text() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let (workspace, _) = project_with_doc(&root);
        let outside = dir.path().join("outside.al");
        std::fs::write(&outside, b"codeunit 50100 Ugly { }").unwrap();

        let params = serde_json::json!({
            "file": outside.to_str().unwrap(),
            "text": "codeunit 50100 Ugly { }",
        });
        let error = super::build_dispatch::dispatch_format(&workspace, 4, &params)
            .await
            .error
            .expect("a write method must refuse supplied text");
        assert_eq!(error.code, error_codes::INVALID_PARAMS, "{error}");
        assert!(error.message.contains("rewrites the file"), "{error}");
    }

    /// `rename` used to read its `uri` through `ensure_document`, which has no
    /// containment check. The file was answered on and stayed in the document
    /// store, so every later read method on the same `uri` was served from it.
    #[tokio::test]
    async fn rename_refuses_a_path_outside_the_project_and_leaves_no_document() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let (workspace, _) = project_with_doc(&root);
        let workspace = std::sync::Arc::new(workspace);
        let outside = dir.path().join("id_rsa");
        std::fs::write(&outside, b"PRIVATE KEY").unwrap();
        let uri = url::Url::from_file_path(&outside).unwrap();
        let shutdown = Notify::new();

        let request = Request::new(
            1,
            "rename",
            Some(serde_json::json!({
                "uri": uri,
                "line": 0,
                "character": 0,
                "newName": "x",
            })),
        );
        let error = dispatch_request(&workspace, request, &shutdown)
            .await
            .error
            .expect("a path outside the project must be refused");
        assert_eq!(error.code, error_codes::PATH_NOT_AUTHORIZED, "{error}");
        assert!(
            !workspace.documents.contains(&uri),
            "the refused file must not be left in the document store"
        );
    }

    #[tokio::test]
    async fn dispatch_ping_returns_pong() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = Notify::new();
        let req = Request::new(7, "ping", None);
        let resp = dispatch_request(&ws, req, &shutdown).await;
        assert_eq!(resp.id, 7);
        assert!(resp.error.is_none());
        assert_eq!(resp.result, Some(serde_json::json!("pong")));
    }

    #[tokio::test]
    async fn dispatch_unknown_method_is_method_not_found() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = Notify::new();
        let req = Request::new(11, "definitelyNotAMethod", None);
        let resp = dispatch_request(&ws, req, &shutdown).await;
        assert_eq!(resp.id, 11);
        assert!(resp.result.is_none());
        let err = resp.error.expect("unknown method must yield an error");
        assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);
        assert!(
            err.message.contains("definitelyNotAMethod"),
            "message should name the method: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn dispatch_status_reports_pid() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = Notify::new();
        let req = Request::new(3, "status", None);
        let resp = dispatch_request(&ws, req, &shutdown).await;
        assert_eq!(resp.id, 3);
        assert!(resp.error.is_none());
        let result = resp.result.expect("status must return a result");
        assert_eq!(
            result.get("pid").and_then(|v| v.as_u64()),
            Some(u64::from(std::process::id()))
        );
        assert_eq!(
            result.get("indexedSymbols").and_then(|v| v.as_u64()),
            Some(0)
        );
        assert!(
            result
                .get("semanticCache")
                .is_some_and(serde_json::Value::is_object),
            "a healthy cache must report concrete statistics"
        );
    }

    /// A client whose request is blocked on the dependency source index needs
    /// to be able to see that from a second connection, or a timeout carries
    /// no reason and the natural response is a retry into the next one.
    #[tokio::test]
    async fn status_reports_dependency_source_index_progress() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = Notify::new();
        let resp = dispatch_request(&ws, Request::new(4, "status", None), &shutdown).await;
        let result = resp.result.expect("status must return a result");
        let source_index = result
            .get("sourceIndex")
            .expect("status must carry sourceIndex");
        assert_eq!(
            source_index.get("state").and_then(|v| v.as_str()),
            Some("idle"),
            "nothing has needed the index yet: {source_index}"
        );
        for key in ["packagesDone", "packagesTotal", "filesDone", "elapsedMs"] {
            assert!(
                source_index.get(key).is_some_and(|v| v.is_u64()),
                "sourceIndex must report {key}: {source_index}"
            );
        }

        // Building it on a workspace with no packages is instant and must
        // leave the counters in the ready state a client waits for.
        let _graph = ws.get_or_build_call_graph().expect("empty graph builds");
        let after = dispatch_request(&ws, Request::new(5, "status", None), &shutdown)
            .await
            .result
            .expect("result");
        assert_eq!(
            after
                .get("sourceIndex")
                .and_then(|index| index.get("state"))
                .and_then(|v| v.as_str()),
            Some("ready")
        );
    }

    #[tokio::test]
    async fn diag_summary_reports_dependency_source_index_progress() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = Notify::new();
        let resp = dispatch_request(&ws, Request::new(6, "diag", None), &shutdown).await;
        let result = resp.result.expect("diag must return a result");
        assert!(
            result.get("sourceIndex").is_some(),
            "diag/summary must carry sourceIndex: {result}"
        );
    }

    /// The daemon that reached 2.9 GB resident reported small totals for every
    /// structure it owns, because the allocator was holding the rest. Both
    /// answers now carry what the operating system sees.
    #[tokio::test]
    async fn status_and_diag_report_resident_memory() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = Notify::new();

        let status = dispatch_request(&ws, Request::new(7, "status", None), &shutdown)
            .await
            .result
            .expect("status must return a result");
        let memory = status.get("memory").expect("status must carry memory");
        assert!(
            memory.get("residentBytes").is_some() && memory.get("peakResidentBytes").is_some(),
            "both resident figures must be present, null where unavailable: {memory}"
        );

        let diag = dispatch_request(&ws, Request::new(8, "diag", None), &shutdown)
            .await
            .result
            .expect("diag must return a result");
        assert!(
            diag.get("process")
                .and_then(|process| process.get("residentBytes"))
                .is_some(),
            "diag/summary must carry the process footprint: {diag}"
        );
    }

    #[tokio::test]
    async fn dispatch_status_rejects_poisoned_inventory_locks() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let poison_target = std::sync::Arc::clone(&ws);
        let _ = std::thread::spawn(move || {
            let _guard = poison_target
                .semantic_cache
                .write()
                .expect("lock starts healthy");
            panic!("poison semantic-cache lock for status regression");
        })
        .join();

        let shutdown = Notify::new();
        let response = dispatch_request(&ws, Request::new(4, "status", None), &shutdown).await;
        let error = response
            .error
            .expect("poisoned status inventory must not be reported as empty");
        assert_eq!(error.code, error_codes::INTERNAL_ERROR);
        assert!(error.message.contains("poisoned"), "got: {}", error.message);
    }

    #[tokio::test]
    async fn dispatch_shutdown_signals_notify_and_acks() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = Notify::new();
        let notified = shutdown.notified();
        tokio::pin!(notified);
        assert!(
            notified.as_mut().now_or_never().is_none(),
            "notify should not be pre-signalled"
        );

        let req = Request::new(99, "shutdown", None);
        let resp = dispatch_request(&ws, req, &shutdown).await;
        assert_eq!(resp.id, 99);
        assert!(resp.error.is_none());
        assert_eq!(
            resp.result,
            Some(serde_json::json!({"shutdownRequested": true}))
        );

        assert!(
            notified.as_mut().now_or_never().is_some(),
            "shutdown must have signalled the Notify"
        );
    }

    #[tokio::test]
    async fn dispatch_diag_defaults_to_summary_when_params_absent() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = Notify::new();
        let req = Request::new(5, "diag", None);
        let resp = dispatch_request(&ws, req, &shutdown).await;
        assert_eq!(resp.id, 5);
        assert!(
            resp.error.is_none(),
            "diag summary must succeed: {:?}",
            resp.error
        );
        assert!(resp.result.is_some());
    }

    #[test]
    fn dispatch_diag_summary_serializes_memory_stats() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let resp = dispatch_diag(&ws, 1, &serde_json::json!({ "cmd": "summary" }));
        assert_eq!(resp.id, 1);
        assert!(resp.error.is_none());
        let result = resp.result.expect("summary must return memory stats");
        assert!(
            result.is_object(),
            "memory stats serialize to a JSON object"
        );
    }

    #[test]
    fn dispatch_diag_rejects_unknown_subcommand() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let resp = dispatch_diag(&ws, 2, &serde_json::json!({ "cmd": "bogus" }));
        assert_eq!(resp.id, 2);
        assert!(resp.result.is_none());
        let err = resp.error.expect("unknown subcommand must error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("bogus"));
    }

    #[test]
    fn dispatch_diag_rejects_non_string_subcommand() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let resp = dispatch_diag(&ws, 3, &serde_json::json!({ "cmd": false }));
        let err = resp.error.expect("wrong-type subcommand must error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("'cmd'"));
    }

    #[test]
    fn parse_object_kind_rejects_garbage_with_invalid_params() {
        // A non-AL kind string must map to an INVALID_PARAMS error Response
        // that names the bad input — never panic, never default silently.
        let err = parse_object_kind(8, "notakind").expect_err("garbage kind must be rejected");
        assert_eq!(err.id, 8);
        let rpc = err.error.expect("must carry an RpcError");
        assert_eq!(rpc.code, error_codes::INVALID_PARAMS);
        assert!(rpc.message.contains("notakind"));
    }

    #[test]
    fn require_project_root_errors_when_no_project_loaded() {
        // A fresh workspace has no project; require_project_root must return
        // an INTERNAL_ERROR Response rather than a path.
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let err = require_project_root(&ws, 4).expect_err("no project => Err");
        assert_eq!(err.id, 4);
        let rpc = err.error.expect("must carry an RpcError");
        assert_eq!(rpc.code, error_codes::INTERNAL_ERROR);
    }

    /// JSON-RPC 2.0 ids may be strings; the daemon must dispatch them and echo
    /// the id back unchanged instead of answering `-32700`.
    #[tokio::test]
    async fn string_and_null_request_ids_are_dispatched_and_echoed() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = Notify::new();

        let request: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":"call-7","method":"ping"}"#)
                .expect("a string id must deserialize");
        let id = request.id.clone().expect("id present");
        let response = dispatch_request(&ws, request, &shutdown).await;
        let frame = response.to_json_with_id(&id);
        assert_eq!(frame["id"], serde_json::json!("call-7"));
        assert_eq!(frame["result"], serde_json::json!("pong"));

        // A negative id is legal JSON-RPC but does not fit the dispatcher's
        // u64, so it must be echoed verbatim rather than answered with 0.
        let request: Request = serde_json::from_str(r#"{"jsonrpc":"2.0","id":-3,"method":"ping"}"#)
            .expect("a negative id must deserialize");
        let id = request.id.clone().expect("id present");
        assert!(id.as_u64().is_none());
        let response = dispatch_request(&ws, request, &shutdown).await;
        assert_eq!(response.to_json_with_id(&id)["id"], serde_json::json!(-3));

        let request: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#)
                .expect("a null id must deserialize");
        assert!(
            !request.is_notification(),
            "an explicit null id is a request, not a notification"
        );
        let id = request.id.clone().expect("id present");
        let response = dispatch_request(&ws, request, &shutdown).await;
        assert!(response.to_json_with_id(&id)["id"].is_null());
    }

    #[test]
    fn in_flight_guard_tracks_dispatch_and_restores_on_drop() {
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        {
            let _first = super::InFlightGuard::new(&counter);
            assert_eq!(counter.load(std::sync::atomic::Ordering::Acquire), 1);
            {
                let _second = super::InFlightGuard::new(&counter);
                assert_eq!(counter.load(std::sync::atomic::Ordering::Acquire), 2);
            }
            assert_eq!(
                counter.load(std::sync::atomic::Ordering::Acquire),
                1,
                "a finished request must release its in-flight slot"
            );
        }
        assert_eq!(
            counter.load(std::sync::atomic::Ordering::Acquire),
            0,
            "the idle reaper must see zero once every request completes"
        );
    }

    #[test]
    fn a_message_without_an_id_is_a_notification() {
        let notification: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"ping"}"#).expect("valid request");
        assert!(notification.is_notification());
    }

    /// Valid JSON that is not a valid request object is `-32600`, not `-32700`.
    #[test]
    fn malformed_request_objects_are_invalid_request_not_parse_error() {
        // Valid JSON, but `method` is missing.
        let value: serde_json::Value =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":4}"#).expect("valid JSON");
        assert!(serde_json::from_value::<Request>(value).is_err());
        // Whereas this is not JSON at all.
        assert!(serde_json::from_str::<serde_json::Value>("{not json").is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn project_root_wait_reports_busy_instead_of_no_project() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        // Hold the project write lock for longer than the wait window.
        let holder = std::sync::Arc::clone(&ws);
        let guard = holder.project.write().await;
        let ws_for_task = std::sync::Arc::clone(&ws);
        let probe =
            tokio::task::spawn_blocking(move || super::project_root_with_wait(&ws_for_task));
        let error = probe.await.expect("probe joins").expect_err("lock held");
        assert!(error.contains("busy"), "unexpected error: {error}");
        drop(guard);

        // Once released, the same call reports "no project loaded" (Ok(None)),
        // which is a different condition from "busy".
        let ws_for_task = std::sync::Arc::clone(&ws);
        let resolved =
            tokio::task::spawn_blocking(move || super::project_root_with_wait(&ws_for_task))
                .await
                .expect("probe joins")
                .expect("lock is free");
        assert!(resolved.is_none());
    }

    // al-workspace's core initializer uses `block_in_place`, so this needs the
    // multi-threaded flavor (the daemon itself always runs multi-threaded).
    #[tokio::test(flavor = "multi_thread")]
    async fn oversized_workspace_files_are_skipped_instead_of_aborting_startup() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            serde_json::json!({
                "id": "00000000-0000-0000-0000-0000000000aa",
                "name": "Skip test",
                "publisher": "Tests",
                "version": "1.0.0.0",
                "dependencies": [],
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Small.Codeunit.al"),
            "codeunit 50100 Small\n{\n}\n",
        )
        .unwrap();
        let big = "codeunit 50101 Big\n{\n}\n".to_string() + &" ".repeat(4096);
        std::fs::write(dir.path().join("Big.Codeunit.al"), &big).unwrap();

        let ws = al_workspace::Workspace::new();
        ws.config.write().await.max_document_size_bytes = Some(128);
        super::initialize_daemon_workspace(&ws, dir.path())
            .await
            .expect("one oversized file must not abort daemon startup");

        let small = url::Url::from_file_path(dir.path().join("Small.Codeunit.al")).unwrap();
        assert!(
            ws.documents.contains(&small),
            "the rest of the workspace must still be queryable"
        );
        let oversized = url::Url::from_file_path(dir.path().join("Big.Codeunit.al")).unwrap();
        assert!(
            !ws.documents.contains(&oversized),
            "the oversized file must be skipped, not opened"
        );
    }

    #[test]
    fn ensure_document_loads_file_from_disk_then_serves_text() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("On.al");
        std::fs::write(&file, b"codeunit 50000 Foo {}").unwrap();
        let uri = url::Url::from_file_path(&file).unwrap();

        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        assert!(ws.documents.get_text(&uri).is_none());

        // ensure_document uses block_in_place, which requires a multi-thread
        // runtime context.
        let rt = tokio::runtime::Builder::new_multi_thread().build().unwrap();
        let text = rt.block_on(async { require_document_text(&ws, &uri, 1).await });
        assert_eq!(text.unwrap(), "codeunit 50000 Foo {}");
        assert_eq!(
            ws.documents.get_text(&uri).as_deref(),
            Some("codeunit 50000 Foo {}")
        );
    }

    #[test]
    fn require_document_text_returns_file_not_found_for_missing_file() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let uri = url::Url::parse("file:///no/such/al-file-xyz.al").unwrap();
        let rt = tokio::runtime::Builder::new_multi_thread().build().unwrap();
        let err = rt.block_on(async { require_document_text(&ws, &uri, 6).await.unwrap_err() });
        assert_eq!(err.id, 6);
        let rpc = err.error.expect("must carry an RpcError");
        assert_eq!(rpc.code, error_codes::FILE_NOT_FOUND);
    }

    #[test]
    fn ensure_document_returns_invalid_params_for_non_file_uri() {
        // A non-file URI has no filesystem path; ensure_document must return
        // a concrete protocol error rather than silently failing.
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let uri = url::Url::parse("https://example.com/x.al").unwrap();
        let rt = tokio::runtime::Builder::new_multi_thread().build().unwrap();
        let response = rt
            .block_on(async { ensure_document(&ws, &uri, 7) })
            .expect_err("non-file URI must be rejected");
        assert_eq!(response.id, 7);
        assert_eq!(
            response.error.expect("RPC error").code,
            error_codes::INVALID_PARAMS
        );
    }

    #[test]
    fn extract_uri_parses_valid_file_url() {
        let params = serde_json::json!({ "uri": "file:///a/b.al" });
        let uri = extract_uri(&params).expect("a valid file URL must parse");
        assert_eq!(uri.as_str(), "file:///a/b.al");
        assert_eq!(uri.scheme(), "file");
    }

    #[test]
    fn extract_uri_returns_none_when_uri_missing_or_unparseable() {
        assert!(extract_uri(&serde_json::json!({})).is_none());
        assert!(extract_uri(&serde_json::json!({ "uri": 42 })).is_none());
        // Present string but not a parseable URL (no scheme → relative-ref error).
        assert!(extract_uri(&serde_json::json!({ "uri": "not a url" })).is_none());
    }

    #[test]
    fn extract_position_parses_valid_line_and_character() {
        let params = serde_json::json!({ "line": 12, "character": 34 });
        let pos = extract_position(&params).expect("valid coords must parse");
        assert_eq!(pos.line, 12);
        assert_eq!(pos.character, 34);
    }

    #[test]
    fn extract_position_returns_none_when_a_field_is_missing() {
        assert!(extract_position(&serde_json::json!({ "line": 1 })).is_none());
        assert!(extract_position(&serde_json::json!({ "character": 1 })).is_none());
        assert!(extract_position(&serde_json::json!({})).is_none());
    }

    #[test]
    fn extract_position_rejects_character_overflow() {
        // line in range, character out of u32 range — must reject the whole
        // position rather than truncate the character.
        let params = serde_json::json!({ "line": 0, "character": (u32::MAX as u64) + 1 });
        assert!(extract_position(&params).is_none());
    }

    #[test]
    fn invalid_params_carries_invalid_params_code_and_id() {
        let resp = invalid_params(42);
        assert_eq!(resp.id, 42);
        assert!(resp.result.is_none());
        let err = resp.error.expect("invalid_params must carry an error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(!err.message.is_empty());
    }

    #[test]
    fn file_not_found_carries_file_not_found_code_and_id() {
        let resp = file_not_found(13);
        assert_eq!(resp.id, 13);
        assert!(resp.result.is_none());
        let err = resp.error.expect("file_not_found must carry an error");
        assert_eq!(err.code, error_codes::FILE_NOT_FOUND);
    }

    #[test]
    fn rpc_error_preserves_code_message_and_id() {
        let resp = rpc_error(77, error_codes::INTERNAL_ERROR, "boom");
        assert_eq!(resp.id, 77);
        assert!(resp.result.is_none());
        let err = resp.error.expect("rpc_error must carry an error");
        assert_eq!(err.code, error_codes::INTERNAL_ERROR);
        assert_eq!(err.message, "boom");
    }

    /// `rules` is a static query (lint rule catalogue) needing no project; it
    /// must route through `dispatch_request` and return a JSON array result
    /// with no error. The catalogue combines file-local, transaction,
    /// native-check, and workspace-native rule registries; this test pins the
    /// transport shape while the registry-specific test pins its contents.
    #[tokio::test]
    async fn dispatch_rules_returns_array_result() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = Notify::new();
        let resp = dispatch_request(&ws, Request::new(21, "rules", None), &shutdown).await;
        assert_eq!(resp.id, 21);
        assert!(resp.error.is_none(), "rules must succeed: {:?}", resp.error);
        assert!(
            resp.result.expect("rules must return a result").is_array(),
            "rules result must be a JSON array"
        );
    }

    #[tokio::test]
    async fn dispatch_packages_returns_empty_array_on_fresh_workspace() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = Notify::new();
        let resp = dispatch_request(&ws, Request::new(22, "packages", None), &shutdown).await;
        assert_eq!(resp.id, 22);
        assert!(resp.error.is_none());
        let arr = resp.result.expect("packages result").as_array().cloned();
        assert_eq!(arr, Some(vec![]));
    }

    /// `entrypoints` builds the insight graph lazily; on a fresh (empty)
    /// workspace it must still succeed and return a JSON array.
    #[tokio::test]
    async fn dispatch_entrypoints_succeeds_on_empty_workspace() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = Notify::new();
        let resp = dispatch_request(&ws, Request::new(23, "entrypoints", None), &shutdown).await;
        assert_eq!(resp.id, 23);
        assert!(resp.error.is_none());
        assert!(resp.result.expect("entrypoints result").is_array());
    }

    /// `hover` requires uri + position; with null params (the default when the
    /// wire omits `params`) it must route through and surface INVALID_PARAMS,
    /// proving both the routing entry and the shared `invalid_params` helper.
    #[tokio::test]
    async fn dispatch_hover_without_params_is_invalid_params() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = Notify::new();
        let resp = dispatch_request(&ws, Request::new(31, "hover", None), &shutdown).await;
        assert_eq!(resp.id, 31);
        assert!(resp.result.is_none());
        let err = resp.error.expect("missing hover params must error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
    }

    /// Routing for synchronous LSP methods that also validate params. Each must
    /// be reachable via `dispatch_request` and return INVALID_PARAMS for the
    /// empty-params case — this covers a swath of the routing table at once.
    #[tokio::test]
    async fn dispatch_param_validating_methods_route_and_reject_empty_params() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = Notify::new();
        for method in [
            "definition",
            "references",
            "implementations",
            "signatureHelp",
            "rename",
            "documentSymbols",
            "foldingRanges",
            "semanticTokens",
            "lint",
            "format",
            "trace",
        ] {
            let req = Request::new(40, method, None);
            let resp = dispatch_request(&ws, req, &shutdown).await;
            assert_eq!(resp.id, 40, "{method}: id must be preserved");
            assert!(
                resp.result.is_none(),
                "{method}: empty params must not yield a result"
            );
            let err = resp
                .error
                .unwrap_or_else(|| panic!("{method}: empty params must error"));
            assert_eq!(
                err.code,
                error_codes::INVALID_PARAMS,
                "{method}: expected INVALID_PARAMS, got code {}",
                err.code
            );
        }
    }

    /// `object` requires a `kind` param; an unknown kind string must route
    /// through `parse_object_kind` and surface INVALID_PARAMS naming the input.
    #[tokio::test]
    async fn dispatch_object_with_bad_kind_is_invalid_params() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = Notify::new();
        let params = serde_json::json!({ "kind": "notakind", "name": "X" });
        let req = Request::new(45, "object", Some(params));
        let resp = dispatch_request(&ws, req, &shutdown).await;
        assert_eq!(resp.id, 45);
        let err = resp.error.expect("bad kind must error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("notakind"));
    }

    /// The request id must be threaded through to the response for the
    /// not-found path too — a regression here would mismatch client futures.
    #[tokio::test]
    async fn dispatch_preserves_request_id_on_unknown_method() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let shutdown = Notify::new();
        let resp = dispatch_request(&ws, Request::new(9_999, "nope.nope", None), &shutdown).await;
        assert_eq!(resp.id, 9_999);
        assert_eq!(
            resp.error.expect("unknown must error").code,
            error_codes::METHOD_NOT_FOUND
        );
    }
}
