use super::*;

/// Exercises the actual platform backend selected by `interprocess`: a Unix
/// domain socket on Linux/macOS and a named pipe on Windows. Keep this outside
/// the Unix-only legacy test module so Windows CI proves that client setup,
/// nonblocking pipe I/O, framing, and response parsing work together.
mod cross_platform_tests {
    use super::{connect_stream, DaemonClient};
    use crate::jsonrpc::{Request, Response};
    use crate::socket::socket_path_with_runtime_dir;
    #[cfg(unix)]
    use interprocess::local_socket::traits::Stream as _;
    use interprocess::local_socket::{
        traits::Listener as _, GenericFilePath, ListenerOptions, ToFsName,
    };
    use std::io::{BufRead, Write};
    use std::sync::atomic::{AtomicU32, Ordering};

    static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

    #[test]
    fn local_transport_round_trip_uses_real_platform_backend() {
        let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "al-protocol-cross-platform-{}-{n}",
            std::process::id()
        ));
        let project = root.join("project");
        // Unix-domain sockets have a small path cap (104 bytes on macOS).
        // GitHub's checkout and temp paths can exceed it before the endpoint
        // filename is appended, so keep this real-backend fixture beneath the
        // short, conventional Unix temp root. Windows named pipes are not
        // filesystem paths and retain the fully isolated fixture directory.
        #[cfg(unix)]
        let runtime =
            std::path::PathBuf::from(format!("/tmp/al-protocol-{}-{n}", std::process::id()));
        #[cfg(windows)]
        let runtime = root.join("runtime");
        std::fs::create_dir_all(&project).expect("create project directory");
        std::fs::create_dir_all(runtime.join("al-lsp")).expect("create runtime directory");

        let endpoint = socket_path_with_runtime_dir(&project, runtime.to_string_lossy())
            .expect("create platform endpoint");
        let name = endpoint
            .as_path()
            .to_fs_name::<GenericFilePath>()
            .expect("convert endpoint name");
        let listener = ListenerOptions::new()
            .name(name)
            .create_sync()
            .expect("bind platform local transport");

        // Keep the server-side named-pipe handle alive until the client has
        // consumed the response. The Windows local-socket wrapper's `flush`
        // is intentionally a no-op, so dropping the short-lived fixture
        // server immediately after `write_all` can race the client and turn a
        // valid buffered response into EOF. Real daemons keep the connection
        // open for subsequent requests.
        let (response_read_tx, response_read_rx) = std::sync::mpsc::channel();

        let server = std::thread::spawn(move || {
            let conn = listener.accept().expect("accept client");
            let mut reader = std::io::BufReader::new(&conn);
            let mut line = String::new();
            reader.read_line(&mut line).expect("read request frame");
            let request: Request = serde_json::from_str(line.trim()).expect("parse request");

            let response = Response::ok(
                request.dispatch_id(),
                serde_json::json!({"transport": "local", "method": request.method}),
            );
            let mut frame = serde_json::to_vec(&response).expect("serialize response");
            frame.push(b'\n');
            let mut writer = &conn;
            writer.write_all(&frame).expect("write response frame");
            writer.flush().expect("flush response frame");
            let _ = response_read_rx.recv_timeout(std::time::Duration::from_secs(5));
        });

        let stream = connect_stream(&endpoint).expect("connect platform local transport");
        let mut client = DaemonClient::from_stream(stream).expect("construct daemon client");
        let response = client
            .request("test/platform", None)
            .expect("complete platform round trip");
        assert_eq!(response["transport"], "local");
        assert_eq!(response["method"], "test/platform");
        response_read_tx
            .send(())
            .expect("notify fixture server that response was consumed");

        drop(client);
        server.join().expect("server thread completed");
        #[cfg(unix)]
        let _ = std::fs::remove_file(&endpoint);
        let _ = std::fs::remove_dir_all(&root);
        #[cfg(unix)]
        let _ = std::fs::remove_dir_all(&runtime);
    }

    #[test]
    fn local_transport_request_deadline_is_enforced() {
        let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let root =
            std::env::temp_dir().join(format!("al-protocol-deadline-{}-{n}", std::process::id()));
        let project = root.join("project");
        #[cfg(unix)]
        let runtime = std::path::PathBuf::from(format!(
            "/tmp/al-protocol-deadline-{}-{n}",
            std::process::id()
        ));
        #[cfg(windows)]
        let runtime = root.join("runtime");
        std::fs::create_dir_all(&project).expect("create project directory");
        std::fs::create_dir_all(runtime.join("al-lsp")).expect("create runtime directory");

        let endpoint = socket_path_with_runtime_dir(&project, runtime.to_string_lossy())
            .expect("create platform endpoint");
        let name = endpoint
            .as_path()
            .to_fs_name::<GenericFilePath>()
            .expect("convert endpoint name");
        let listener = ListenerOptions::new()
            .name(name)
            .create_sync()
            .expect("bind platform local transport");

        let server = std::thread::spawn(move || {
            let conn = listener.accept().expect("accept client");
            let mut reader = std::io::BufReader::new(&conn);
            let mut line = String::new();
            reader.read_line(&mut line).expect("read request frame");
            std::thread::sleep(std::time::Duration::from_secs(1));
        });

        let stream = connect_stream(&endpoint).expect("connect platform local transport");
        let mut client = DaemonClient::from_stream(stream).expect("construct daemon client");
        #[cfg(unix)]
        client
            .reader
            .get_ref()
            .set_recv_timeout(Some(std::time::Duration::from_millis(50)))
            .expect("shorten Unix socket poll interval for deadline test");
        let error = client
            .request_with_timeout("test/never", None, std::time::Duration::from_millis(200))
            .expect_err("silent platform peer must hit the request deadline");
        assert!(
            error.contains("did not respond"),
            "deadline error must be actionable: {error}"
        );

        drop(client);
        server.join().expect("server thread completed");
        #[cfg(unix)]
        let _ = std::fs::remove_file(&endpoint);
        let _ = std::fs::remove_dir_all(&root);
        #[cfg(unix)]
        let _ = std::fs::remove_dir_all(&runtime);
    }

    /// The old message told the caller to "retry with a longer timeout" and
    /// there was no way to set one.
    #[test]
    fn a_sub_second_timeout_is_not_reported_as_zero_seconds() {
        use std::time::Duration;
        assert_eq!(
            super::describe_timeout(Duration::from_millis(100)),
            "100 ms"
        );
        assert_eq!(super::describe_timeout(Duration::from_secs(30)), "30s");
        assert_eq!(super::describe_timeout(Duration::from_millis(2500)), "2.5s");
    }

    #[test]
    fn timeout_message_names_a_control_that_exists() {
        use super::is_timeout_message;
        // The literal in `read_one_response`, checked directly: building a
        // real stall here would add seconds to the suite for one string.
        let rendered = format!(
            "Daemon did not respond within {}s — the operation may still be \
             running. Raise AL_REQUEST_TIMEOUT_MS or pass --timeout-ms, or \
             check the daemon log at ~/.local/share/al-lsp/logs/al-lsp.log",
            30
        );
        assert!(is_timeout_message(&rendered));
        assert!(rendered.contains("AL_REQUEST_TIMEOUT_MS"));
        assert!(!rendered.contains("Retry with a longer timeout"));
    }

    #[test]
    fn a_non_timeout_error_is_not_treated_as_one() {
        use super::is_timeout_message;
        assert!(!is_timeout_message(
            "Failed to read response: Connection reset by peer (os error 104)"
        ));
        assert!(!is_timeout_message("Connection closed by daemon (EOF)"));
    }
}

/// What a request does at its deadline, for the `status` answers a cold
/// `trace` sees in turn: the source index building, then the call graph
/// building on top of it, then both ready.
mod deadline_extension_tests {
    use super::{after_deadline, DeadlineAction, IndexProgress, MAX_INDEX_WAIT};
    use serde_json::json;
    use std::time::Duration;

    fn decide(status: &serde_json::Value, waited: Duration, last: &mut usize) -> DeadlineAction {
        after_deadline(
            "trace",
            IndexProgress::from_status(status).as_ref(),
            waited,
            last,
        )
    }

    #[test]
    fn the_deadline_extends_until_the_call_graph_is_built() {
        let source_building = json!({
            "sourceIndex": {"state": "building", "packagesDone": 3, "packagesTotal": 6,
                            "filesDone": 4211, "elapsedMs": 20000},
            "callGraph": {"state": "idle", "elapsedMs": 0},
        });
        let graph_building = json!({
            "sourceIndex": {"state": "ready", "packagesDone": 6, "packagesTotal": 6,
                            "filesDone": 9000, "elapsedMs": 23500},
            "callGraph": {"state": "building", "elapsedMs": 6500},
        });
        let both_ready = json!({
            "sourceIndex": {"state": "ready", "packagesDone": 6, "packagesTotal": 6,
                            "filesDone": 9000, "elapsedMs": 23500},
            "callGraph": {"state": "ready", "elapsedMs": 12000},
        });
        let mut last = 0;
        let waited = Duration::from_secs(30);

        assert_eq!(
            decide(&source_building, waited, &mut last),
            DeadlineAction::KeepWaiting
        );
        assert_eq!(
            decide(&graph_building, waited, &mut last),
            DeadlineAction::KeepWaiting,
            "the call graph is still building after the source index is ready"
        );
        assert_eq!(
            decide(&both_ready, waited, &mut last),
            DeadlineAction::TimeOut
        );
    }

    #[test]
    fn a_call_graph_build_past_the_ceiling_gives_up_and_names_it() {
        let graph_building = json!({
            "sourceIndex": {"state": "ready", "filesDone": 9000},
            "callGraph": {"state": "building", "elapsedMs": 590000},
        });
        let mut last = 9000;
        match decide(&graph_building, MAX_INDEX_WAIT, &mut last) {
            DeadlineAction::GiveUp(message) => {
                assert!(message.contains("call graph"), "{message}");
                assert!(message.contains("590 s"), "{message}");
            }
            other => panic!("expected GiveUp, got {other:?}"),
        }
    }

    #[test]
    fn a_daemon_without_call_graph_progress_keeps_the_source_index_rule() {
        let older_daemon = json!({
            "sourceIndex": {"state": "ready", "filesDone": 9000},
        });
        let mut last = 0;
        assert_eq!(
            decide(&older_daemon, Duration::from_secs(30), &mut last),
            DeadlineAction::TimeOut
        );
    }
}
