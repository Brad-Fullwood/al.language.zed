//! Tests for the area A2 review fixes in the editor commands.

use super::*;

/// A2-neon-07: the Run Test lens runs a local test and stores its result,
/// with no launch configuration in the project.
#[tokio::test]
async fn run_test_runs_a_local_test_without_a_launch_configuration() {
    // The client socket is dropped, so a request to the editor fails at once.
    // A socket that is read and never answered leaves a request that is awaited
    // waiting, for example the symbol download prompt of `initialize_workspace`.
    let (service, _) = tower_lsp::LspService::new(crate::server::AlLsp::new);
    let server = service.inner();
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("app.json"),
        r#"{"id":"00000000-0000-0000-0000-000000000001","name":"Lens","publisher":"Test","version":"1.0.0.0"}"#,
    )
    .unwrap();
    let source = "codeunit 50100 \"Lens Tests\"\n{\n    Subtype = Test;\n\n    [Test]\n    procedure AddsUp()\n    var\n        Total: Integer;\n    begin\n        Total := 1 + 1;\n        if Total <> 2 then\n            Error('wrong total');\n    end;\n}\n";
    let path = root.path().join("LensTests.al");
    std::fs::write(&path, source).unwrap();
    crate::server::workspace::initialize_workspace(
        Arc::clone(&server.workspace),
        server.client.clone(),
        Some(Url::from_directory_path(root.path()).unwrap()),
        Some(server.workspace_init_state.clone()),
        None,
    )
    .await
    .expect("workspace initializes");

    let result = run_test(
        server,
        &[serde_json::json!({"codeunitId": 50100, "methodName": "AddsUp"})],
    )
    .await;

    assert_eq!(result["status"], "completed", "{result}");
    assert_eq!(result["totals"]["passed"], 1, "{result}");
    let store = server
        .workspace
        .test_results
        .read()
        .unwrap()
        .clone()
        .expect("the run opens the results store");
    let last = store.last_for(50100, "AddsUp").await.unwrap();
    assert!(last.is_some_and(|record| record.status == al_test::TestStatus::Pass));
}

#[test]
fn a_skipped_lens_run_says_live_business_central() {
    let (level, message) = run_test_message(
        "Posts",
        &serde_json::json!({"total": 1, "passed": 0, "failed": 0, "skipped": 1}),
    );
    assert_eq!(level, MessageType::WARNING);
    assert!(message.contains(al_test::test_runner::LIVE_BC_SKIP_MESSAGE));
}

/// A2-codex-18: a report that runs past its timeout is killed.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_timed_out_report_process_is_killed() {
    let dir = tempfile::tempdir().unwrap();
    let child = report_command(
        std::path::Path::new("sleep"),
        &["30".to_string()],
        dir.path(),
    )
    .spawn()
    .expect("sleep starts");
    let pid = child.id().expect("pid");
    let waited = wait_for_report(child, std::time::Duration::from_millis(100), "sleep 30").await;
    let error = waited.expect_err("sleep 30 outlives the timeout");
    assert!(error.contains("did not finish"), "{error}");

    let mut gone = false;
    for _ in 0..50 {
        let state = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| stat.rsplit(')').next().map(|rest| rest.trim().to_string()));
        if state.as_deref().is_none_or(|rest| rest.starts_with('Z')) {
            gone = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(gone, "the report process was not killed");
}
