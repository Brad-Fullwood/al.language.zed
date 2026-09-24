use super::*;
use al_protocol::jsonrpc::error_codes;
use al_workspace::Workspace;

#[test]
fn live_reason_names_the_disqualifying_reason_not_a_supported_call() {
    use al_test::router::{ClassifyResult, RoutingDecision, RoutingReason};
    let reason = |message: &str| RoutingReason {
        message: message.to_string(),
        file: None,
        line: None,
    };
    let classifications = vec![ClassifyResult {
        codeunit_id: 50110,
        codeunit_name: "Loyalty Test".to_string(),
        method_name: "SetTier".to_string(),
        decision: RoutingDecision::LiveBc,
        reasons: vec![
            reason("calls supported Record.Get"),
            reason("uses record table 'Customer' without a workspace table definition"),
        ],
    }];
    assert_eq!(
        super::live_reason(&classifications).as_deref(),
        Some("uses record table 'Customer' without a workspace table definition")
    );
    assert_eq!(super::live_reason(&[]), None);
}

fn empty_ws() -> Workspace {
    Workspace::new()
}

fn assert_invalid_params(response: Response, expected_message: &str) {
    let error = response.error.expect("expected INVALID_PARAMS response");
    assert_eq!(error.code, error_codes::INVALID_PARAMS);
    assert!(
        error.message.contains(expected_message),
        "expected error containing {expected_message:?}, got: {}",
        error.message
    );
}

async fn install_empty_project(ws: &Workspace, root: &std::path::Path) {
    *ws.project.write().await = Some(al_project::project::AlProject {
        root: root.to_path_buf(),
        app_json: al_project::project::AppManifest {
            id: "00000000-0000-0000-0000-000000000001".to_string(),
            name: "Snapshot Tests".to_string(),
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
    });
}

async fn install_test_result_store(ws: &Workspace, tmp: &tempfile::TempDir) {
    let store = al_test::TestResultStore::open(tmp.path().join("test-results.json"))
        .await
        .expect("open sandboxed test-results store");
    *ws.test_results
        .write()
        .expect("test_results lock must not be poisoned") = Some(std::sync::Arc::new(store));
}

#[test]
fn timeout_param_passes_through_sensible_values_and_absence() {
    assert_eq!(
        optional_timeout_ms(&serde_json::json!({"timeoutMs": 0})).unwrap(),
        Some(0)
    );
    assert_eq!(
        optional_timeout_ms(&serde_json::json!({"timeoutMs": 30_000})).unwrap(),
        Some(30_000)
    );
    assert_eq!(optional_timeout_ms(&serde_json::json!({})).unwrap(), None);
}

#[test]
fn timeout_param_rejects_values_over_max_and_wrong_types() {
    assert_eq!(
        optional_timeout_ms(&serde_json::json!({"timeoutMs": MAX_TIMEOUT_MS})).unwrap(),
        Some(MAX_TIMEOUT_MS)
    );
    for value in [
        serde_json::json!(MAX_TIMEOUT_MS + 1),
        serde_json::json!(u64::MAX),
        serde_json::json!(-1),
        serde_json::json!("30000"),
    ] {
        assert!(optional_timeout_ms(&serde_json::json!({"timeoutMs": value})).is_err());
    }
}

#[test]
fn output_path_accepts_relative_inside_project() {
    let project = tempfile::tempdir().unwrap();
    let resolved =
        resolve_output_path_within_project(std::path::Path::new("out/junit.xml"), project.path());
    assert!(
        resolved.is_some(),
        "relative path inside project must resolve"
    );
}

#[test]
fn output_path_for_nonexistent_nested_file_has_no_trailing_separator() {
    // Regression: the tail-reconstruction loop seeded
    // `tail` with an empty `PathBuf` and `push`ed it, which appended a
    // trailing separator (`junit.xml` -> `junit.xml/`). The resolved path
    // was then treated as a directory, `create_dir_all`'d, and the JUnit /
    // Cobertura write failed silently while the command reported success.
    let project = tempfile::tempdir().unwrap();
    let resolved = resolve_output_path_within_project(
        // Neither `reports/` nor the file exist yet — exercises the loop.
        std::path::Path::new("reports/junit.xml"),
        project.path(),
    )
    .expect("nested path inside project must resolve");
    assert_eq!(
        resolved.file_name().and_then(|n| n.to_str()),
        Some("junit.xml"),
        "resolved path must end in the file name, got {resolved:?}"
    );
    assert!(
        !resolved
            .to_string_lossy()
            .ends_with(std::path::MAIN_SEPARATOR),
        "resolved path must not end with a separator, got {resolved:?}"
    );
}

#[test]
fn output_path_accepts_absolute_inside_project() {
    let project = tempfile::tempdir().unwrap();
    let abs = project.path().canonicalize().unwrap().join("results.xml");
    let resolved = resolve_output_path_within_project(&abs, project.path());
    assert!(
        resolved.is_some(),
        "absolute path inside project must resolve"
    );
}

#[test]
fn output_path_rejects_parent_dir_escape() {
    let project = tempfile::tempdir().unwrap();
    let resolved =
        resolve_output_path_within_project(std::path::Path::new("../escape.xml"), project.path());
    assert!(
        resolved.is_none(),
        "../ escape must be rejected, got {resolved:?}"
    );
}

#[test]
fn output_path_rejects_deep_parent_dir_escape() {
    let project = tempfile::tempdir().unwrap();
    let resolved = resolve_output_path_within_project(
        std::path::Path::new("subdir/../../../etc/passwd"),
        project.path(),
    );
    assert!(resolved.is_none());
}

#[test]
fn output_path_rejects_absolute_outside_project() {
    let project = tempfile::tempdir().unwrap();
    let resolved =
        resolve_output_path_within_project(std::path::Path::new("/etc/hosts"), project.path());
    assert!(
        resolved.is_none(),
        "absolute outside project must be rejected"
    );
}

#[cfg(unix)]
#[test]
fn output_path_rejects_symlink_dir_escape() {
    // Negative: a symlinked directory inside the project that points
    // outside must not let a write target escape, even though the textual
    // path starts with the project root.
    let project = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let link = project.path().join("link");
    std::os::unix::fs::symlink(outside.path(), &link).unwrap();

    let resolved =
        resolve_output_path_within_project(std::path::Path::new("link/evil.xml"), project.path());
    assert!(
        resolved.is_none(),
        "symlinked dir escaping the project must be rejected, got {resolved:?}"
    );
}

#[cfg(unix)]
#[test]
fn output_path_rejects_symlink_file_escape() {
    // Negative: an existing output file that is itself a symlink to an
    // outside location must be rejected (it would be followed by write()).
    let project = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_file = outside.path().join("target.xml");
    std::fs::write(&outside_file, b"x").unwrap();
    let link = project.path().join("results.xml");
    std::os::unix::fs::symlink(&outside_file, &link).unwrap();

    let resolved =
        resolve_output_path_within_project(std::path::Path::new("results.xml"), project.path());
    assert!(
        resolved.is_none(),
        "symlinked output file escaping the project must be rejected, got {resolved:?}"
    );
}

#[cfg(unix)]
#[test]
fn output_path_accepts_symlink_dir_staying_inside() {
    // Positive: a symlink that points to another location *inside* the
    // project must still be accepted, with the canonical target returned.
    let project = tempfile::tempdir().unwrap();
    let real_dir = project.path().join("real_out");
    std::fs::create_dir(&real_dir).unwrap();
    let link = project.path().join("out");
    std::os::unix::fs::symlink(&real_dir, &link).unwrap();

    let resolved =
        resolve_output_path_within_project(std::path::Path::new("out/junit.xml"), project.path());
    assert!(
        resolved.is_some(),
        "symlink staying inside the project must be accepted"
    );
    let canonical_root = project.path().canonicalize().unwrap();
    assert!(resolved.unwrap().starts_with(&canonical_root));
}

/// Pure-logic test codeunits (router decision: Interp) must
/// run on the INTERPRETER — actually executing the [Test] procedures —
/// and must NOT require a BC launch config. Previously everything went
/// through LiveBcMode: `test-run-all` refused without .zed/debug.json
/// and then reported "0/0 passed" for tests the interpreter can run.
#[tokio::test(flavor = "multi_thread")]
async fn run_batch_routes_pure_tests_to_interpreter_without_launch_config() {
    let ws = std::sync::Arc::new(empty_ws());
    let tmp = tempfile::TempDir::new().unwrap();
    // NO .zed/debug.json and NO .vscode/launch.json on purpose.
    {
        let mut guard = ws.project.write().await;
        *guard = Some(al_project::project::AlProject {
            root: tmp.path().to_path_buf(),
            app_json: al_project::project::AppManifest {
                id: String::new(),
                name: "test".into(),
                publisher: "test".into(),
                version: "1.0.0.0".into(),
                dependencies: Vec::new(),
                application: None,
                platform: None,
                runtime: None,
            },
            packages_dir: tmp.path().join(".alpackages"),
            packages: Vec::new(),
            server_configs: Vec::new(),
            launch_config_error: None,
        });
    }
    // A pure-logic test codeunit: 2 [Test] procedures, one passing and
    // one failing, so the totals prove real interpreter execution.
    let src_path = tmp.path().join("PureLogicTest.Codeunit.al");
    let source = r#"codeunit 50110 "Pure Logic Test"
{
Subtype = Test;

[Test]
procedure TestAddition()
var
    Result: Integer;
begin
    Result := 2 + 2;
    if Result <> 4 then
        Error('Expected 4, got %1', Result);
end;

[Test]
procedure TestFails()
begin
    Error('intentional failure');
end;
}
"#;
    std::fs::write(&src_path, source).unwrap();
    ws.file_index.add_file(src_path, source.to_string());

    let resp = dispatch_tests_run_batch(
        &ws,
        7,
        &serde_json::json!({
            "codeunitIds": [50110],
            "codeunitNames": ["Pure Logic Test"],
        }),
    )
    .await;
    assert!(
        resp.error.is_none(),
        "interp-routed tests must run without a launch config: {:?}",
        resp.error
    );
    let result = resp.result.expect("result");
    let totals = &result["totals"];
    assert_eq!(
        totals["total"].as_u64(),
        Some(2),
        "both [Test] procedures must execute: {result}"
    );
    assert_eq!(
        totals["passed"].as_u64(),
        Some(1),
        "TestAddition must pass: {result}"
    );
    assert_eq!(
        totals["failed"].as_u64(),
        Some(1),
        "TestFails must fail: {result}"
    );
    let routing = result["routing"].as_array().expect("routing details");
    assert_eq!(routing.len(), 2, "{result}");
    for route in routing {
        assert_eq!(route["decision"], "interp", "{route}");
        assert_eq!(route["classifiedDecision"], "interp", "{route}");
        assert_eq!(route["runsLocally"], true, "{route}");
        assert_eq!(
            route["execution"], "runs locally on the Rust interpreter",
            "{route}"
        );
        assert!(
            route["reasons"]
                .as_array()
                .is_some_and(|reasons| !reasons.is_empty()),
            "{route}"
        );
    }

    let filtered = dispatch_tests_run_batch(
        &ws,
        8,
        &serde_json::json!({
            "codeunitIds": [50110],
            "codeunitNames": ["Pure Logic Test"],
            "filter": "TestAdd*",
        }),
    )
    .await;
    assert!(filtered.error.is_none(), "{:?}", filtered.error);
    let filtered = filtered.result.expect("filtered result");
    assert_eq!(filtered["totals"]["total"], 1, "{filtered}");
    assert_eq!(filtered["totals"]["passed"], 1, "{filtered}");
    assert_eq!(filtered["totals"]["failed"], 0, "{filtered}");
    let routing = filtered["routing"].as_array().expect("filtered routing");
    assert_eq!(routing.len(), 1, "{filtered}");
    assert_eq!(routing[0]["methodName"], "TestAddition", "{filtered}");
}

/// Workspace-table tests classified as InterpRecord must execute on the
/// in-memory record backend without requiring a BC launch configuration.
#[tokio::test(flavor = "multi_thread")]
async fn run_batch_routes_workspace_records_to_local_runtime() {
    let ws = std::sync::Arc::new(empty_ws());
    let tmp = tempfile::TempDir::new().unwrap();
    {
        let mut guard = ws.project.write().await;
        *guard = Some(al_project::project::AlProject {
            root: tmp.path().to_path_buf(),
            app_json: al_project::project::AppManifest {
                id: String::new(),
                name: "test".into(),
                publisher: "test".into(),
                version: "1.0.0.0".into(),
                dependencies: Vec::new(),
                application: None,
                platform: None,
                runtime: None,
            },
            packages_dir: tmp.path().join(".alpackages"),
            packages: Vec::new(),
            server_configs: Vec::new(),
            launch_config_error: None,
        });
    }

    let table_path = tmp.path().join("NativeEntry.Table.al");
    let table_source = r#"table 50130 "Native Entry"
{
fields
{
    field(1; "No."; Code[20]) { }
    field(2; Amount; Integer) { }
}
keys
{
    key(PK; "No.") { Clustered = true; }
}
}
"#;
    std::fs::write(&table_path, table_source).unwrap();
    ws.file_index.add_file(table_path, table_source.to_string());

    let test_path = tmp.path().join("NativeRecordTest.Codeunit.al");
    let test_source = r#"codeunit 50131 "Native Record Test"
{
Subtype = Test;

[Test]
procedure InsertAndRead()
var
    Entry: Record "Native Entry";
begin
    Entry.Init();
    Entry."No." := 'A';
    Entry.Amount := 42;
    Entry.Insert();
    if Entry.Count() <> 1 then
        Error('expected one record');
    if not Entry.Get('A') then
        Error('record not found');
    if Entry.Amount <> 42 then
        Error('wrong amount');
end;
}
"#;
    std::fs::write(&test_path, test_source).unwrap();
    ws.file_index.add_file(test_path, test_source.to_string());

    let classifications = al_test::router::classify_all(&ws).unwrap();
    assert_eq!(
        classifications.len(),
        1,
        "unexpected classifications: {classifications:?}"
    );
    assert_eq!(
        classifications[0].decision,
        al_test::router::RoutingDecision::InterpRecord,
        "record test must select the local record backend: {classifications:?}"
    );

    let resp = dispatch_tests_run_batch(
        &ws,
        8,
        &serde_json::json!({
            "codeunitIds": [50131],
            "codeunitNames": ["Native Record Test"],
        }),
    )
    .await;
    assert!(
        resp.error.is_none(),
        "InterpRecord tests must run without a launch config: {:?}",
        resp.error
    );
    let result = resp.result.expect("result");
    assert_eq!(result["totals"]["total"].as_u64(), Some(1), "{result}");
    assert_eq!(result["totals"]["passed"].as_u64(), Some(1), "{result}");
    assert_eq!(result["totals"]["failed"].as_u64(), Some(0), "{result}");
    assert_eq!(result["routing"][0]["decision"], "interpRecord", "{result}");
    assert_eq!(result["routing"][0]["runsLocally"], true, "{result}");
    assert!(
        result["routing"][0]["reasons"]
            .as_array()
            .is_some_and(|reasons| !reasons.is_empty()),
        "{result}"
    );

    let single = dispatch_tests_run(
        &ws,
        9,
        &serde_json::json!({
            "codeunit": 50131,
            "codeunitName": "Native Record Test",
            "method": "InsertAndRead",
        }),
    )
    .await;
    assert!(
        single.error.is_none(),
        "single InterpRecord run must use the same local route: {:?}",
        single.error
    );
    let single_result = single.result.expect("single result");
    assert_eq!(single_result["result"]["total"].as_u64(), Some(1));
    assert_eq!(single_result["result"]["passed"].as_u64(), Some(1));
}

/// Build a workspace rooted at `tmp` with no launch config (so interp tests
/// run locally) and a single pure-logic test codeunit containing a branch.
/// Returns the workspace plus the 1-based source lines of the taken and
/// not-taken branch bodies, for coverage assertions.
async fn ws_with_branch_codeunit(tmp: &tempfile::TempDir) -> (std::sync::Arc<Workspace>, u32, u32) {
    let ws = std::sync::Arc::new(empty_ws());
    {
        let mut guard = ws.project.write().await;
        *guard = Some(al_project::project::AlProject {
            root: tmp.path().to_path_buf(),
            app_json: al_project::project::AppManifest {
                id: String::new(),
                name: "test".into(),
                publisher: "test".into(),
                version: "1.0.0.0".into(),
                dependencies: Vec::new(),
                application: None,
                platform: None,
                runtime: None,
            },
            packages_dir: tmp.path().join(".alpackages"),
            packages: Vec::new(),
            server_configs: Vec::new(),
            launch_config_error: None,
        });
    }
    let src_path = tmp.path().join("CovBatch.Codeunit.al");
    let source = r#"codeunit 50120 "Cov Batch Test"
{
Subtype = Test;

[Test]
procedure TestBranch()
var
    x: Integer;
begin
    x := 1;
    if x = 1 then
        x := 100
    else
        x := 200;
end;
}
"#;
    std::fs::write(&src_path, source).unwrap();
    ws.file_index.add_file(src_path, source.to_string());
    let line_of = |needle: &str| -> u32 {
        source
            .lines()
            .position(|l| l.contains(needle))
            .map(|i| i as u32 + 1)
            .unwrap()
    };
    (ws, line_of("x := 100"), line_of("x := 200"))
}

/// With `coverage: true`, an interp-routed run must surface a
/// `coverage` object of per-file executed lines (taken branch present,
/// not-taken absent) AND write a DYNAMIC-mode Cobertura document.
#[tokio::test(flavor = "multi_thread")]
async fn run_batch_with_coverage_surfaces_executed_lines_and_dynamic_cobertura() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (ws, taken, untaken) = ws_with_branch_codeunit(&tmp).await;

    let resp = dispatch_tests_run_batch(
        &ws,
        70,
        &serde_json::json!({
            "codeunitIds": [50120],
            "codeunitNames": ["Cov Batch Test"],
            "coverage": true,
            "coberturaOut": "cov.xml",
        }),
    )
    .await;
    assert!(
        resp.error.is_none(),
        "coverage run failed: {:?}",
        resp.error
    );
    let result = resp.result.expect("result");

    let coverage = result
        .get("coverage")
        .expect("coverage object must be present when requested");
    assert_eq!(coverage["mode"], "dynamic-executed-lines");
    let files = coverage["files"].as_array().expect("files array");
    assert_eq!(files.len(), 1, "one covered file expected: {coverage}");
    let exec: Vec<u64> = files[0]["executedLines"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_u64())
        .collect();
    assert!(
        exec.contains(&(taken as u64)),
        "taken branch line {taken} must be executed; got {exec:?}"
    );
    assert!(
        !exec.contains(&(untaken as u64)),
        "not-taken branch line {untaken} must NOT be executed; got {exec:?}"
    );

    // The Cobertura file on disk must be the DYNAMIC variant, not static.
    let cov_path = tmp.path().canonicalize().unwrap().join("cov.xml");
    let xml = std::fs::read_to_string(&cov_path)
        .unwrap_or_else(|e| panic!("cobertura file {cov_path:?} unreadable: {e}"));
    assert!(
        xml.contains(r#"coverage-mode="dynamic-executed-lines""#),
        "dynamic cobertura expected, got:\n{xml}"
    );
    assert!(
        !xml.contains("static-call-graph"),
        "must not be static:\n{xml}"
    );
}

/// With coverage off, the result must carry no
/// `coverage` key and `coberturaOut` must produce the STATIC document — proving
/// the existing behaviour is untouched.
#[tokio::test(flavor = "multi_thread")]
async fn run_batch_without_coverage_is_unchanged_static() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (ws, _taken, _untaken) = ws_with_branch_codeunit(&tmp).await;

    let resp = dispatch_tests_run_batch(
        &ws,
        71,
        &serde_json::json!({
            "codeunitIds": [50120],
            "codeunitNames": ["Cov Batch Test"],
            "coberturaOut": "cov.xml",
        }),
    )
    .await;
    assert!(resp.error.is_none(), "run failed: {:?}", resp.error);
    let result = resp.result.expect("result");
    assert!(
        result.get("coverage").is_none(),
        "coverage key must be absent when not requested: {result}"
    );

    let cov_path = tmp.path().canonicalize().unwrap().join("cov.xml");
    let xml = std::fs::read_to_string(&cov_path).expect("cobertura file");
    assert!(
        xml.contains(r#"coverage-mode="static-call-graph""#),
        "static cobertura expected by default, got:\n{xml}"
    );
    assert!(
        !xml.contains("dynamic-executed-lines"),
        "default run must not emit a dynamic doc:\n{xml}"
    );
}

#[tokio::test]
async fn run_batch_no_project_returns_error() {
    let ws = std::sync::Arc::new(empty_ws());
    let resp = dispatch_tests_run_batch(&ws, 1, &serde_json::json!({})).await;
    assert!(
        resp.error.is_some(),
        "expected error response with no project"
    );
}

#[tokio::test]
async fn run_auto_no_project_returns_error() {
    let ws = std::sync::Arc::new(empty_ws());
    let resp = dispatch_tests_run_auto(&ws, 2, &serde_json::json!({})).await;
    assert!(resp.error.is_some());
}

#[tokio::test]
async fn last_results_no_project_returns_error() {
    let ws = empty_ws();
    let resp = dispatch_tests_last_results(&ws, 3, &serde_json::json!({})).await;
    assert!(resp.error.is_some());
}

#[tokio::test]
async fn run_batch_missing_codeunit_ids_is_invalid_params() {
    // Set a project root so we get past the NO_PROJECT check, then
    // miss codeunitIds — must return INVALID_PARAMS, not crash.
    let ws = std::sync::Arc::new(empty_ws());
    let tmp = tempfile::TempDir::new().unwrap();
    let dot_zed = tmp.path().join(".zed");
    std::fs::create_dir_all(&dot_zed).unwrap();
    std::fs::write(
        dot_zed.join("debug.json"),
        r#"[{"name":"local","type":"al","request":"launch","environmentType":"OnPrem","server":"http://localhost","serverInstance":"BC","authentication":"UserPassword"}]"#,
    )
    .unwrap();
    {
        let mut guard = ws.project.write().await;
        *guard = Some(al_project::project::AlProject {
            root: tmp.path().to_path_buf(),
            app_json: al_project::project::AppManifest {
                id: String::new(),
                name: "test".into(),
                publisher: "test".into(),
                version: "1.0.0.0".into(),
                dependencies: Vec::new(),
                application: None,
                platform: None,
                runtime: None,
            },
            packages_dir: tmp.path().join(".alpackages"),
            packages: Vec::new(),
            server_configs: Vec::new(),
            launch_config_error: None,
        });
    }
    let resp = dispatch_tests_run_batch(&ws, 4, &serde_json::json!({})).await;
    let err = resp
        .error
        .expect("expected error response for missing codeunitIds");
    assert!(
        err.message.contains("codeunitIds"),
        "error must mention the missing parameter; got: {err:?}"
    );
}

#[tokio::test]
async fn run_batch_rejects_out_of_range_codeunit_id() {
    let ws = std::sync::Arc::new(empty_ws());
    let tmp = tempfile::TempDir::new().unwrap();
    let dot_zed = tmp.path().join(".zed");
    std::fs::create_dir_all(&dot_zed).unwrap();
    std::fs::write(
        dot_zed.join("debug.json"),
        r#"[{"name":"local","type":"al","request":"launch","environmentType":"OnPrem","server":"http://localhost","serverInstance":"BC","authentication":"UserPassword"}]"#,
    )
    .unwrap();
    {
        let mut guard = ws.project.write().await;
        *guard = Some(al_project::project::AlProject {
            root: tmp.path().to_path_buf(),
            app_json: al_project::project::AppManifest {
                id: String::new(),
                name: "test".into(),
                publisher: "test".into(),
                version: "1.0.0.0".into(),
                dependencies: Vec::new(),
                application: None,
                platform: None,
                runtime: None,
            },
            packages_dir: tmp.path().join(".alpackages"),
            packages: Vec::new(),
            server_configs: Vec::new(),
            launch_config_error: None,
        });
    }
    let resp = dispatch_tests_run_batch(
        &ws,
        6,
        &serde_json::json!({ "codeunitIds": [(i32::MAX as i64) + 1] }),
    )
    .await;
    let err = resp
        .error
        .expect("expected error response for out-of-range codeunit id");
    assert_eq!(err.code, al_protocol::jsonrpc::error_codes::INVALID_PARAMS);
    assert!(err.message.contains("out of range"), "got: {}", err.message);
}

#[tokio::test]
async fn run_batch_rejects_malformed_parallel_arrays_and_options_before_project_access() {
    let ws = std::sync::Arc::new(empty_ws());
    let malformed = [
        (serde_json::json!({"codeunitIds": "50100"}), "codeunitIds"),
        (
            serde_json::json!({"codeunitIds": [50100], "codeunitNames": "Tests"}),
            "codeunitNames",
        ),
        (
            serde_json::json!({"codeunitIds": [50100], "codeunitNames": []}),
            "one entry",
        ),
        (
            serde_json::json!({"codeunitIds": [50100], "codeunitNames": [null]}),
            "non-empty string",
        ),
        (
            serde_json::json!({"codeunitIds": [50100], "methodNames": "TestOne"}),
            "methodNames",
        ),
        (
            serde_json::json!({"codeunitIds": [50100], "methodNames": []}),
            "string or null",
        ),
        (
            serde_json::json!({"codeunitIds": [50100], "methodNames": [false]}),
            "non-empty string or null",
        ),
        (
            serde_json::json!({"codeunitIds": [50100], "parallel": "true"}),
            "parallel",
        ),
        (
            serde_json::json!({"codeunitIds": [50100], "coverage": 1}),
            "coverage",
        ),
        (
            serde_json::json!({"codeunitIds": [50100], "timeoutMs": "30000"}),
            "timeoutMs",
        ),
        (
            serde_json::json!({"codeunitIds": [50100], "filter": false}),
            "filter",
        ),
        (
            serde_json::json!({"codeunitIds": [50100], "junitOut": []}),
            "junitOut",
        ),
        (
            serde_json::json!({"codeunitIds": [50100], "coberturaOut": {}}),
            "coberturaOut",
        ),
        (
            serde_json::json!({"codeunitIds": [50100], "config": 7}),
            "config",
        ),
        (
            serde_json::json!({
                "codeunitIds": [50100, 50100],
                "methodNames": ["TestOne", "testone"]
            }),
            "duplicate test request",
        ),
    ];

    for (index, (params, expected_message)) in malformed.into_iter().enumerate() {
        let response = dispatch_tests_run_batch(&ws, index as u64, &params).await;
        assert_invalid_params(response, expected_message);
    }
}

#[tokio::test]
async fn last_results_returns_empty_when_no_history() {
    let ws = empty_ws();
    let tmp = tempfile::TempDir::new().unwrap();
    install_test_result_store(&ws, &tmp).await;
    {
        let mut guard = ws.project.write().await;
        *guard = Some(al_project::project::AlProject {
            root: tmp.path().to_path_buf(),
            app_json: al_project::project::AppManifest {
                id: String::new(),
                name: "test".into(),
                publisher: "test".into(),
                version: "1.0.0.0".into(),
                dependencies: Vec::new(),
                application: None,
                platform: None,
                runtime: None,
            },
            packages_dir: tmp.path().join(".alpackages"),
            packages: Vec::new(),
            server_configs: Vec::new(),
            launch_config_error: None,
        });
    }
    let resp = dispatch_tests_last_results(&ws, 5, &serde_json::json!({})).await;
    assert!(
        resp.error.is_none(),
        "fresh project should succeed, got error: {:?}",
        resp.error
    );
    let results = resp
        .result
        .as_ref()
        .and_then(|v| v.get("results"))
        .and_then(|v| v.as_array())
        .expect("expected results array");
    assert!(results.is_empty(), "fresh history must be empty");
}

/// Build a workspace with a fresh, sandboxed test-results store so the
/// last_results dispatcher reaches its codeunitId validation.
async fn ws_with_project(tmp: &tempfile::TempDir) -> Workspace {
    let ws = empty_ws();
    let mut guard = ws.project.write().await;
    *guard = Some(al_project::project::AlProject {
        root: tmp.path().to_path_buf(),
        app_json: al_project::project::AppManifest {
            id: String::new(),
            name: "test".into(),
            publisher: "test".into(),
            version: "1.0.0.0".into(),
            dependencies: Vec::new(),
            application: None,
            platform: None,
            runtime: None,
        },
        packages_dir: tmp.path().join(".alpackages"),
        packages: Vec::new(),
        server_configs: Vec::new(),
        launch_config_error: None,
    });
    drop(guard);
    install_test_result_store(&ws, tmp).await;
    ws
}

#[tokio::test]
async fn last_results_single_lookup_rejects_out_of_range_codeunit_id() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ws = ws_with_project(&tmp).await;
    let resp = dispatch_tests_last_results(
        &ws,
        7,
        &serde_json::json!({
            "codeunitId": (i32::MAX as i64) + 1,
            "methodName": "TestFoo",
        }),
    )
    .await;
    let err = resp
        .error
        .expect("expected error response for out-of-range codeunitId");
    assert_eq!(err.code, al_protocol::jsonrpc::error_codes::INVALID_PARAMS);
    assert!(err.message.contains("out of range"), "got: {}", err.message);
}

#[tokio::test]
async fn last_results_bulk_filter_rejects_out_of_range_codeunit_id() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ws = ws_with_project(&tmp).await;
    let resp = dispatch_tests_last_results(
        &ws,
        8,
        &serde_json::json!({ "codeunitId": (i32::MIN as i64) - 1 }),
    )
    .await;
    let err = resp
        .error
        .expect("expected error response for out-of-range codeunitId");
    assert_eq!(err.code, al_protocol::jsonrpc::error_codes::INVALID_PARAMS);

    assert!(err.message.contains("out of range"), "got: {}", err.message);
}

#[tokio::test]
async fn last_results_rejects_malformed_lookup_parameters_before_project_access() {
    let ws = empty_ws();
    for (index, (params, expected_message)) in [
        (serde_json::json!({"codeunitId": "50100"}), "codeunitId"),
        (
            serde_json::json!({"codeunitId": 50100, "methodName": false}),
            "methodName",
        ),
        (
            serde_json::json!({"codeunitId": 50100, "methodName": "  "}),
            "methodName",
        ),
        (
            serde_json::json!({"methodName": "TestOne"}),
            "requires 'codeunitId'",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let response = dispatch_tests_last_results(&ws, index as u64, &params).await;
        assert_invalid_params(response, expected_message);
    }
}

#[test]
fn freeze_test_codeunit_result_wire_format() {
    use al_test::result::{TestCodeunitResult, TestMethodResult, TestStatus};
    let v = TestCodeunitResult::from_methods(
        "X".to_string(),
        42,
        vec![TestMethodResult {
            name: "M".to_string(),
            status: TestStatus::Pass,
            error: None,
            duration_ms: Some(10),
            failure_kind: None,
        }],
    );
    let json = serde_json::to_value(&v).unwrap();
    for key in [
        "name", "id", "methods", "total", "passed", "failed", "skipped",
    ] {
        assert!(
            json.get(key).is_some(),
            "wire-format key `{key}` missing — DO NOT rename without bumping schema_version"
        );
    }
    let m = &json["methods"][0];
    for key in ["name", "status", "durationMs"] {
        assert!(m.get(key).is_some(), "TestMethodResult key `{key}` missing");
    }
    assert_eq!(json["methods"][0]["status"], "pass");
}

#[test]
fn freeze_test_coverage_report_wire_format() {
    use al_analysis::queries::test_coverage::{
        CoverageReport, CoveredProcedure, TestCoverageEntry,
    };
    let r = CoverageReport {
        coverage: vec![TestCoverageEntry {
            codeunit: "TestCU".to_string(),
            test_procedure: "TestProc".to_string(),
            covers: vec![CoveredProcedure {
                name: "DoWork".to_string(),
                object: "MyCU".to_string(),
                file: "src/MyCU.al".to_string(),
                line: 10,
            }],
            unresolved_calls: Vec::new(),
        }],
        untested: Vec::new(),
    };
    let json = serde_json::to_value(&r).unwrap();
    for key in ["coverage", "untested"] {
        assert!(
            json.get(key).is_some(),
            "CoverageReport key `{key}` missing"
        );
    }
    let entry = &json["coverage"][0];
    for key in ["codeunit", "testProcedure", "covers", "unresolvedCalls"] {
        assert!(
            entry.get(key).is_some(),
            "TestCoverageEntry key `{key}` missing"
        );
    }
    let cov = &entry["covers"][0];
    for key in ["name", "object", "file", "line"] {
        assert!(
            cov.get(key).is_some(),
            "CoveredProcedure key `{key}` missing"
        );
    }
}

#[test]
fn freeze_test_codeunit_discovery_wire_format() {
    use al_analysis::queries::tests::{TestCodeunit, TestProcedure};
    let v = TestCodeunit {
        id: 50100,
        name: "MyTests".to_string(),
        file: "src/MyTests.al".to_string(),
        tests: vec![TestProcedure {
            name: "TestA".to_string(),
            line: 5,
            handler_functions: vec![],
        }],
        test_initializers: vec![],
        test_cleanups: vec![],
    };
    let json = serde_json::to_value(&v).unwrap();
    for key in [
        "id",
        "name",
        "file",
        "tests",
        "testInitializers",
        "testCleanups",
    ] {
        assert!(json.get(key).is_some(), "TestCodeunit key `{key}` missing");
    }
    let proc = &json["tests"][0];
    for key in ["name", "line", "handlerFunctions"] {
        assert!(proc.get(key).is_some(), "TestProcedure key `{key}` missing");
    }
}

#[tokio::test]
async fn run_batch_persistence_roundtrip_via_dispatchers() {
    let ws = empty_ws();
    let tmp = tempfile::TempDir::new().unwrap();
    install_test_result_store(&ws, &tmp).await;
    {
        let mut guard = ws.project.write().await;
        *guard = Some(al_project::project::AlProject {
            root: tmp.path().to_path_buf(),
            app_json: al_project::project::AppManifest {
                id: String::new(),
                name: "test".into(),
                publisher: "test".into(),
                version: "1.0.0.0".into(),
                dependencies: Vec::new(),
                application: None,
                platform: None,
                runtime: None,
            },
            packages_dir: tmp.path().join(".alpackages"),
            packages: Vec::new(),
            server_configs: Vec::new(),
            launch_config_error: None,
        });
    }

    let store = ws
        .test_results
        .read()
        .ok()
        .and_then(|g| g.clone())
        .expect("store should be initialised");
    store
        .append(al_test::persistence::TestRunRecord {
            timestamp: 1_700_000_000,
            codeunit_id: 50200,
            codeunit_name: "Persisted".into(),
            method_name: "TestRoundtrip".into(),
            status: al_test::result::TestStatus::Pass,
            duration_ms: Some(7),
            error: None,
        })
        .await
        .unwrap();

    let resp =
        dispatch_tests_last_results(&ws, 99, &serde_json::json!({"codeunitId": 50200})).await;
    assert!(resp.error.is_none(), "got error: {:?}", resp.error);
    let results = resp
        .result
        .as_ref()
        .and_then(|v| v.get("results"))
        .and_then(|v| v.as_array())
        .expect("results array");
    assert_eq!(results.len(), 1, "the appended record must be visible");
    assert_eq!(results[0]["methodName"], "TestRoundtrip");
    assert_eq!(results[0]["status"], "pass");
}

#[test]
fn affected_missing_changed_files_returns_invalid_params() {
    let ws = empty_ws();
    let resp = dispatch_tests_affected(&ws, 1, &serde_json::json!({}));
    let err = resp.error.expect("expected error");
    assert!(err.message.contains("changedFiles"));
}

#[test]
fn affected_empty_list_returns_empty_response() {
    let ws = empty_ws();
    let resp = dispatch_tests_affected(&ws, 2, &serde_json::json!({ "changedFiles": [] }));
    assert!(resp.error.is_none());
    let arr = resp
        .result
        .as_ref()
        .and_then(|v| v.get("affected"))
        .and_then(|v| v.as_array())
        .expect("affected array");
    assert!(arr.is_empty());
}

#[test]
fn affected_rejects_malformed_or_duplicate_paths() {
    let ws = empty_ws();
    for (index, (params, expected_message)) in [
        (
            serde_json::json!({"changedFiles": "src/Test.al"}),
            "changedFiles",
        ),
        (
            serde_json::json!({"changedFiles": ["src/Test.al", 7]}),
            "changedFiles[1]",
        ),
        (
            serde_json::json!({"changedFiles": ["src/Test.al", "src/Test.al"]}),
            "duplicates",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        assert_invalid_params(
            dispatch_tests_affected(&ws, index as u64, &params),
            expected_message,
        );
    }
}

#[test]
fn classify_empty_workspace_returns_empty_classifications() {
    let ws = empty_ws();
    let resp = dispatch_tests_classify(&ws, 3);
    assert!(resp.error.is_none());
    let arr = resp
        .result
        .as_ref()
        .and_then(|v| v.get("classifications"))
        .and_then(|v| v.as_array())
        .expect("classifications array");
    assert!(arr.is_empty(), "no test codeunits → no classifications");
}

#[test]
fn freeze_routing_decision_wire_format() {
    // Wire format freeze for the routing strings — used by CLI,
    // TUI, and CodeLens. DO NOT rename without bumping schema_version.
    use al_test::router::RoutingDecision;
    assert_eq!(RoutingDecision::Interp.as_str(), "interp");
    assert_eq!(RoutingDecision::InterpRecord.as_str(), "interpRecord");
    assert_eq!(RoutingDecision::LiveBc.as_str(), "liveBc");
}

#[test]
fn routing_details_report_actual_codeunit_backend_and_aggregation_reason() {
    use al_test::router::{ClassifyResult, RoutingDecision, RoutingReason};
    use al_test::session::TestId;

    let classifications = vec![
        ClassifyResult {
            codeunit_id: 50100,
            codeunit_name: "Mixed Tests".to_string(),
            method_name: "PureMethod".to_string(),
            decision: RoutingDecision::Interp,
            reasons: Vec::new(),
        },
        ClassifyResult {
            codeunit_id: 50100,
            codeunit_name: "Mixed Tests".to_string(),
            method_name: "HttpMethod".to_string(),
            decision: RoutingDecision::LiveBc,
            reasons: vec![RoutingReason {
                message: "uses HttpClient".to_string(),
                file: Some("Mixed.Codeunit.al".to_string()),
                line: Some(20),
            }],
        },
    ];
    let requested = vec![TestId {
        codeunit_id: 50100,
        codeunit_name: "Mixed Tests".to_string(),
        method_name: None,
    }];

    let routing = test_routing_details(&classifications, &requested);
    assert_eq!(routing.len(), 2);
    assert_eq!(routing[0]["classifiedDecision"], "interp");
    assert_eq!(routing[0]["decision"], "liveBc");
    assert_eq!(routing[0]["runsLocally"], false);
    assert!(routing[0]["reasons"]
        .as_array()
        .is_some_and(|reasons| reasons.iter().any(|reason| {
            reason["message"]
                .as_str()
                .is_some_and(|message| message.contains("shared state"))
        })));
    assert_eq!(routing[1]["classifiedDecision"], "liveBc");
    assert_eq!(routing[1]["decision"], "liveBc");
    assert_eq!(routing[1]["reasons"][0]["message"], "uses HttpClient");
}

#[test]
fn freeze_affected_test_wire_format() {
    use al_analysis::queries::tests::AffectedTest;
    let v = AffectedTest {
        codeunit_id: 50100,
        codeunit_name: "X".into(),
        method_name: "M".into(),
        file: "src/X.al".into(),
        line: 7,
    };
    let json = serde_json::to_value(&v).unwrap();
    for key in ["codeunitId", "codeunitName", "methodName", "file", "line"] {
        assert!(json.get(key).is_some(), "AffectedTest key `{key}` missing");
    }
}

#[tokio::test]
async fn freeze_run_batch_response_shape() {
    // run_batch with no project produces an error envelope; the shape
    // we pin is the SUCCESS envelope, so synthesize one directly.
    let summary = al_test::result::TestCodeunitResult::from_methods("Cu".into(), 1, vec![]);
    let summaries_json = serde_json::to_value(&[summary]).unwrap();
    let response = serde_json::json!({
        "summaries": summaries_json,
        "routing": [],
        "totals": { "total": 0_u64, "passed": 0_u64, "failed": 0_u64, "skipped": 0_u64 },
    });
    for key in ["summaries", "routing", "totals"] {
        assert!(response.get(key).is_some(), "run_batch key `{key}` missing");
    }
    for key in ["total", "passed", "failed", "skipped"] {
        assert!(
            response["totals"].get(key).is_some(),
            "totals key `{key}` missing"
        );
    }
}

#[tokio::test]
async fn freeze_last_results_response_shapes() {
    let ws = empty_ws();
    let tmp = tempfile::TempDir::new().unwrap();
    install_test_result_store(&ws, &tmp).await;
    {
        let mut g = ws.project.write().await;
        *g = Some(al_project::project::AlProject {
            root: tmp.path().to_path_buf(),
            app_json: al_project::project::AppManifest {
                id: String::new(),
                name: "test".into(),
                publisher: "test".into(),
                version: "1.0.0.0".into(),
                dependencies: Vec::new(),
                application: None,
                platform: None,
                runtime: None,
            },
            packages_dir: tmp.path().join(".alpackages"),
            packages: Vec::new(),
            server_configs: Vec::new(),
            launch_config_error: None,
        });
    }
    let bulk = dispatch_tests_last_results(&ws, 100, &serde_json::json!({})).await;
    let r = bulk.result.expect("ok");
    assert!(
        r.get("results").is_some(),
        "tests.last_results bulk shape must expose `results`"
    );

    // Specific (codeunit, method) lookup: shape = { "lastResult": null|{} }.
    let specific = dispatch_tests_last_results(
        &ws,
        101,
        &serde_json::json!({ "codeunitId": 1, "methodName": "X" }),
    )
    .await;
    let r = specific.result.expect("ok");
    assert!(
        r.get("lastResult").is_some(),
        "tests.last_results specific shape must expose `lastResult`"
    );
}

#[test]
fn freeze_classify_response_shape() {
    let ws = empty_ws();
    let resp = dispatch_tests_classify(&ws, 200);
    let r = resp.result.expect("ok");
    assert!(
        r.get("classifications").is_some(),
        "tests.classify shape must expose `classifications`"
    );

    let item = serde_json::json!({
        "codeunitId": 1,
        "codeunitName": "X",
        "methodName": "M",
        "decision": "interp",
        "runsLocally": true,
        "execution": "runs locally on the Rust interpreter",
        "reasons": [{ "message": "", "file": null, "line": null }],
    });
    for key in [
        "codeunitId",
        "codeunitName",
        "methodName",
        "decision",
        "runsLocally",
        "execution",
        "reasons",
    ] {
        assert!(
            item.get(key).is_some(),
            "classification key `{key}` missing"
        );
    }
    for key in ["message", "file", "line"] {
        assert!(
            item["reasons"][0].get(key).is_some(),
            "reason key `{key}` missing"
        );
    }
}

#[test]
fn freeze_affected_response_shape() {
    let ws = empty_ws();
    let resp = dispatch_tests_affected(&ws, 300, &serde_json::json!({ "changedFiles": [] }));
    let r = resp.result.expect("ok");
    assert!(
        r.get("affected").is_some(),
        "tests.affected shape must expose `affected`"
    );
    assert!(
        r["affected"].as_array().expect("array").is_empty(),
        "empty changedFiles must yield empty affected"
    );
}

#[tokio::test]
async fn snapshot_validate_missing_path_is_invalid_params() {
    let ws = empty_ws();
    let resp = dispatch_tests_snapshot_validate(&ws, 1, &serde_json::json!({})).await;
    let err = resp.error.expect("err");
    assert_eq!(err.code, error_codes::INVALID_PARAMS);
    assert!(err.message.contains("snapshotPath"));
}

#[tokio::test]
async fn snapshot_capture_requires_explicit_live_metadata_before_network_io() {
    let ws = std::sync::Arc::new(empty_ws());
    let resp = dispatch_tests_snapshot_capture(&ws, 1, &serde_json::json!({})).await;
    let err = resp.error.expect("err");
    assert_eq!(err.code, error_codes::INVALID_PARAMS);
    assert!(err.message.contains("codeunitId"));

    let resp = dispatch_tests_snapshot_capture(
        &ws,
        2,
        &serde_json::json!({
            "codeunitId": 50100,
            "codeunitName": "Snapshot Tests",
            "methodName": "Captures",
        }),
    )
    .await;
    let err = resp.error.expect("err");
    assert_eq!(err.code, error_codes::INVALID_PARAMS);
    assert!(
        err.message.contains("bcVersion"),
        "capture must not invent runtime version metadata: {}",
        err.message
    );
}

#[tokio::test]
async fn snapshot_validate_unreadable_path_is_invalid_params() {
    let ws = empty_ws();
    let tmp = tempfile::TempDir::new().unwrap();
    install_empty_project(&ws, tmp.path()).await;
    let resp = dispatch_tests_snapshot_validate(
        &ws,
        2,
        &serde_json::json!({ "snapshotPath": "missing.snap.json" }),
    )
    .await;
    let err = resp.error.expect("err");
    assert_eq!(err.code, error_codes::INVALID_PARAMS);
    assert!(err.message.contains("resolve snapshot path"));
}

#[tokio::test]
async fn snapshot_replay_requires_baseline_and_runtime_metadata() {
    let ws = std::sync::Arc::new(empty_ws());
    let missing_path = dispatch_tests_snapshot_replay(&ws, 1, &serde_json::json!({})).await;
    let err = missing_path.error.expect("missing path must fail");
    assert_eq!(err.code, error_codes::INVALID_PARAMS);
    assert!(err.message.contains("snapshotPath"));

    let missing_version = dispatch_tests_snapshot_replay(
        &ws,
        2,
        &serde_json::json!({ "snapshotPath": "baseline.snap.json" }),
    )
    .await;
    let err = missing_version.error.expect("missing version must fail");
    assert_eq!(err.code, error_codes::INVALID_PARAMS);
    assert!(err.message.contains("bcVersion"));
}

#[tokio::test]
async fn snapshot_diff_missing_paths_is_invalid_params() {
    let ws = empty_ws();
    let only_a = dispatch_tests_snapshot_diff(&ws, 1, &serde_json::json!({ "pathA": "/x" })).await;
    let err = only_a.error.expect("missing pathB must error");
    assert_eq!(err.code, error_codes::INVALID_PARAMS);
    assert!(err.message.contains("pathB"));

    let none = dispatch_tests_snapshot_diff(&ws, 2, &serde_json::json!({})).await;
    let err = none.error.expect("missing pathA must error");
    assert!(err.message.contains("pathA"));
}

#[tokio::test]
async fn tests_mutate_no_project_returns_error() {
    let ws = std::sync::Arc::new(empty_ws());
    let resp = dispatch_tests_mutate(&ws, 1, &serde_json::json!({})).await;
    let err = resp.error.expect("no project must error");
    assert_eq!(err.code, error_codes::INTERNAL_ERROR);
    assert!(
        err.message.contains("project"),
        "error must reference the missing project: {}",
        err.message
    );
}

#[tokio::test]
async fn tests_mutate_rejects_malformed_options_before_project_access() {
    let ws = std::sync::Arc::new(empty_ws());
    for (index, (params, expected_message)) in [
        (serde_json::json!({"parallel": 1}), "parallel"),
        (serde_json::json!({"timeoutMs": -1}), "timeoutMs"),
        (serde_json::json!({"files": "src"}), "files"),
        (serde_json::json!({"files": []}), "must not be empty"),
        (
            serde_json::json!({"files": ["src/Test.al", null]}),
            "files[1]",
        ),
        (
            serde_json::json!({"files": ["src/Test.al", "src/Test.al"]}),
            "duplicates",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let response = dispatch_tests_mutate(&ws, index as u64, &params).await;
        assert_invalid_params(response, expected_message);
    }
}

#[test]
fn mutation_allowlist_resolves_relative_and_absolute_paths_to_index_keys() {
    let project = tempfile::tempdir().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let file = src.join("Mutation.Codeunit.al");
    std::fs::write(&file, "codeunit 50100 Mutation { }").unwrap();

    let ws = empty_ws();
    ws.file_index
        .add_file(file.clone(), "codeunit 50100 Mutation { }".to_string());

    let relative = resolve_mutation_file_allowlist(
        &ws,
        project.path(),
        vec!["src/Mutation.Codeunit.al".to_string()],
    )
    .expect("relative workspace path");
    assert_eq!(relative, vec![file.to_string_lossy().into_owned()]);

    let absolute = resolve_mutation_file_allowlist(
        &ws,
        project.path(),
        vec![file.to_string_lossy().into_owned()],
    )
    .expect("absolute workspace path");
    assert_eq!(absolute, vec![file.to_string_lossy().into_owned()]);
}

#[test]
fn mutation_allowlist_rejects_alias_duplicates_outside_and_unindexed_files() {
    let project = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let indexed = src.join("Mutation.Codeunit.al");
    let unindexed = src.join("Unindexed.Codeunit.al");
    let outside_file = outside.path().join("Outside.Codeunit.al");
    std::fs::write(&indexed, "codeunit 50100 Mutation { }").unwrap();
    std::fs::write(&unindexed, "codeunit 50101 Unindexed { }").unwrap();
    std::fs::write(&outside_file, "codeunit 50102 Outside { }").unwrap();

    let ws = empty_ws();
    ws.file_index
        .add_file(indexed, "codeunit 50100 Mutation { }".to_string());

    let duplicate = resolve_mutation_file_allowlist(
        &ws,
        project.path(),
        vec![
            "src/Mutation.Codeunit.al".to_string(),
            "src/./Mutation.Codeunit.al".to_string(),
        ],
    )
    .expect_err("path aliases must not select one file twice");
    assert!(duplicate.contains("same file"), "{duplicate}");

    let not_indexed = resolve_mutation_file_allowlist(
        &ws,
        project.path(),
        vec!["src/Unindexed.Codeunit.al".to_string()],
    )
    .expect_err("existing non-indexed files must be rejected");
    assert!(not_indexed.contains("not an indexed AL workspace file"));

    let escaped = resolve_mutation_file_allowlist(
        &ws,
        project.path(),
        vec![outside_file.to_string_lossy().into_owned()],
    )
    .expect_err("out-of-project files must be rejected");
    assert!(escaped.contains("outside the loaded project"));
}

#[tokio::test(flavor = "multi_thread")]
async fn tests_mutate_accepts_relative_workspace_file_selection() {
    let project = tempfile::tempdir().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let file = src.join("PureLogicTest.Codeunit.al");
    let source = r#"codeunit 50110 "Pure Logic Test"
{
Subtype = Test;

[Test]
procedure TestAddition()
var
    Result: Integer;
begin
    Result := 2 + 2;
    if Result <> 4 then
        Error('Expected 4, got %1', Result);
end;
}
"#;
    std::fs::write(&file, source).unwrap();

    let ws = std::sync::Arc::new(empty_ws());
    install_empty_project(&ws, project.path()).await;
    ws.file_index.add_file(file, source.to_string());

    let response = dispatch_tests_mutate(
        &ws,
        1,
        &serde_json::json!({
            "files": ["src/PureLogicTest.Codeunit.al"],
            "timeoutMs": 10_000,
        }),
    )
    .await;
    assert!(
        response.error.is_none(),
        "relative selection must run: {:?}",
        response.error
    );
    let result = response.result.expect("mutation report");
    assert!(
        result["variants"]
            .as_array()
            .is_some_and(|variants| !variants.is_empty()),
        "{result}"
    );
    assert_eq!(result["executorPhase"], "interpreter", "{result}");
}
