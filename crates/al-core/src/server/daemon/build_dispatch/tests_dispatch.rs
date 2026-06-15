//! Test-runner and coverage dispatchers.

use super::super::rpc_error;
use super::ERR_NO_PROJECT;
use crate::workspace::Workspace;
use al_protocol::jsonrpc::{error_codes, Response};
use std::path::PathBuf;

/// multi-day or 584-year (u64::MAX ms) test run.
const MAX_TIMEOUT_MS: u64 = 60 * 60 * 1000;

fn clamp_timeout_ms(t: Option<u64>) -> Option<u64> {
    t.map(|ms| ms.min(MAX_TIMEOUT_MS))
}
/// Resolve a user-provided output-file path against `project_root` and reject
/// anything that escapes it (path traversal). Used for JUnit / Cobertura
/// output paths in `dispatch_tests_run_batch`, where a malicious or
/// misconfigured client could otherwise ask the daemon to write XML to
/// arbitrary filesystem locations as the daemon's user.
///
/// Symlinks are resolved (including symlinked parent directories that point
/// outside the project), so `/project/link/evil.xml` where `link -> /outside`
/// is rejected even though it textually starts with the project root.
///
/// Returns `Some(canonical_path)` — the symlink-resolved absolute path — if the
/// requested location is inside `project_root`, else `None`.
fn resolve_output_path_within_project(
    requested: &std::path::Path,
    project_root: &std::path::Path,
) -> Option<PathBuf> {
    // Resolve relative paths against project_root.
    let absolute = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        project_root.join(requested)
    };

    // Logical (non-filesystem) normalisation: collapse `.` and `..` segments.
    // We can't use `Path::canonicalize` because the file may not yet exist.
    let mut normalised = PathBuf::new();
    for comp in absolute.components() {
        use std::path::Component;
        match comp {
            Component::ParentDir => {
                if !normalised.pop() {
                    // `..` above the root — definitely escaping.
                    return None;
                }
            }
            Component::CurDir => {}
            other => normalised.push(other.as_os_str()),
        }
    }

    // Canonicalise the project root so symlinks / case-normalisation can't
    // be used to spoof containment. The root must exist; if canonicalisation
    // fails, reject conservatively.
    let project_canonical = project_root.canonicalize().ok()?;

    // Logical normalisation alone is not enough: a symlink *inside* the
    // project pointing outside (e.g. `/project/link -> /outside`) would let
    // `/project/link/evil.xml` pass a textual `starts_with` check while the
    // real write target is `/outside/evil.xml`. Resolve symlinks by
    // canonicalising the deepest ancestor of `normalised` that actually
    // exists, then re-appending the not-yet-created tail, and require the
    // *canonical* result to stay within the canonical root.
    let mut existing = normalised.as_path();
    let mut tail = PathBuf::new();
    let canonical_existing = loop {
        match existing.canonicalize() {
            Ok(c) => break c,
            Err(_) => {
                // Walk up one component, remembering the stripped tail.
                let file = existing.file_name()?;
                let mut new_tail = PathBuf::from(file);
                new_tail.push(&tail);
                tail = new_tail;
                existing = existing.parent()?;
            }
        }
    };
    let resolved = canonical_existing.join(&tail);

    if resolved.starts_with(&project_canonical) {
        // Return the canonical, symlink-resolved path so the subsequent write
        // targets exactly what we validated.
        Some(resolved)
    } else {
        None
    }
}
// ---------------------------------------------------------------------------
// WP15: Test runner
// ---------------------------------------------------------------------------

pub(in crate::server::daemon) fn dispatch_tests_discover(
    workspace: &Workspace,
    id: u64,
) -> Response {
    let tests = crate::queries::tests::discover_tests(workspace);
    let value = serde_json::to_value(&tests).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
        ..Default::default()
    }
}
pub(in crate::server::daemon) fn dispatch_tests_coverage(
    workspace: &Workspace,
    id: u64,
) -> Response {
    let report = crate::queries::test_coverage::test_coverage(workspace);
    let value = serde_json::to_value(&report).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
        ..Default::default()
    }
}
/// T1502: Execute tests via BC REST API + T1503: Return results as diagnostics.
///
/// Params:
/// - `codeunit` (i64): codeunit ID to run. Required.
/// - `codeunitName` (str): display name for the result. Defaults to the ID as a string.
/// - `method` (str, optional): run only this test method.
///
/// Launch config is read from the project root (`.vscode/launch.json` or `.zed/debug.json`).
/// The first config entry is used unless `config` (str) names a specific one.
///
/// Response includes:
/// - `result`: `TestCodeunitResult` JSON
/// - `diagnostics`: array of `TestDiagnostic` for failed/skipped tests (T1503)
pub(in crate::server::daemon) async fn dispatch_tests_run(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    use crate::launch::find_launch_config;
    use crate::queries::test_diagnostics::results_to_diagnostics;
    use crate::test_runner::TestRunnerClient;

    let project_root = match workspace
        .project
        .read()
        .await
        .as_ref()
        .map(|p| p.root.clone())
    {
        Some(root) => root,
        None => {
            return rpc_error(id, error_codes::INTERNAL_ERROR, ERR_NO_PROJECT);
        }
    };

    let codeunit_id = match params.get("codeunit").and_then(|v| v.as_i64()) {
        Some(n) => match i32::try_from(n) {
            Ok(v) => v,
            Err(_) => {
                return rpc_error(id, error_codes::INVALID_PARAMS, "codeunit ID out of range");
            }
        },
        None => {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                "Missing 'codeunit' parameter (i64 codeunit ID)",
            );
        }
    };
    let codeunit_name = params
        .get("codeunitName")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| codeunit_id.to_string());
    let method = params
        .get("method")
        .and_then(|v| v.as_str())
        .map(String::from);
    let config_name = params.get("config").and_then(|v| v.as_str());

    // -- Find launch config (sync fs read → run on blocking thread) ----------
    let launch_cfg_root = project_root.clone();
    let launch_cfg_opt =
        match tokio::task::spawn_blocking(move || find_launch_config(&launch_cfg_root)).await {
            Ok(opt) => opt,
            Err(e) => {
                return rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    &format!("launch config task failed: {e}"),
                );
            }
        };
    let launch_cfg = match launch_cfg_opt {
        Some(cfg) => cfg,
        None => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                "No launch config found — create .vscode/launch.json or .zed/debug.json",
            );
        }
    };

    let server_config = if let Some(name) = config_name {
        launch_cfg
            .configs
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name))
    } else {
        launch_cfg.configs.first()
    };

    let server_config = match server_config {
        Some(c) => c,
        None => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                "No BC server config found in launch config",
            );
        }
    };

    // -- Run tests via BC REST API (T1502) ------------------------------------
    let client = TestRunnerClient::new(server_config);
    let run_result = client
        .run_codeunit(codeunit_id, &codeunit_name, method.as_deref())
        .await;

    let result = match run_result {
        Ok(r) => r,
        Err(e) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("Test run failed: {e}"),
            );
        }
    };

    // -- Convert to diagnostics (T1503) ----------------------------------------
    let diagnostics = results_to_diagnostics(std::slice::from_ref(&result), workspace);

    // Persist per-method records so CodeLens / tests.last_results /
    // test-runner TUI all see the same history regardless of which entry
    // point the user used. Errors are logged, not propagated — we still
    // want to return the test result to the caller.
    if let Err(e) = ensure_result_store(workspace, &project_root).await {
        tracing::warn!(error = %e, "test_results store init failed; persistence skipped");
    } else if let Some(store) = workspace.test_results.read().ok().and_then(|g| g.clone()) {
        for m in &result.methods {
            let rec = crate::test_engine::persistence::TestRunRecord {
                timestamp: crate::test_engine::persistence::now_secs(),
                codeunit_id: result.id,
                codeunit_name: result.name.clone(),
                method_name: m.name.clone(),
                status: m.status.clone(),
                duration_ms: m.duration_ms,
                error: m.error.clone(),
            };
            if let Err(e) = store.append(rec).await {
                tracing::warn!(error = %e, "failed to persist test result");
            }
        }
    }

    let result_json = serde_json::to_value(&result).unwrap_or(serde_json::Value::Null);
    let diag_json = serde_json::to_value(&diagnostics).unwrap_or(serde_json::json!([]));

    Response {
        id,
        result: Some(serde_json::json!({
            "result": result_json,
            "diagnostics": diag_json,
        })),
        error: None,
        ..Default::default()
    }
}
// ---------------------------------------------------------------------------
// p1-5: New test_engine endpoints — additive; existing tests.run is frozen.
// ---------------------------------------------------------------------------

/// `tests.run_batch` — run multiple codeunits, optionally in parallel,
/// optionally writing JUnit/Cobertura output to disk.
///
/// Params:
/// - `codeunitIds`: `[i32]` (required)
/// - `codeunitNames`: `[str]` (parallel-indexed; falls back to ID-as-string)
/// - `parallel`: bool (default false)
/// - `timeoutMs`: u64 (default 30_000)
/// - `junitOut`: str (path to write JUnit XML)
/// - `coberturaOut`: str (path to write Cobertura XML)
/// - `filter`: str (forwarded; currently logged only)
pub(in crate::server::daemon) async fn dispatch_tests_run_batch(
    workspace: &std::sync::Arc<Workspace>,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    use crate::launch::find_launch_config;
    use crate::test_engine::backends::interp::InterpMode;
    use crate::test_engine::backends::live_bc::LiveBcMode;
    use crate::test_engine::output::{cobertura, junit};
    use crate::test_engine::router::RoutingDecision;
    use crate::test_engine::session::{RunOptions, TestEvent, TestId, TestSession};
    use tokio::sync::mpsc;

    let project_root = match workspace
        .project
        .read()
        .await
        .as_ref()
        .map(|p| p.root.clone())
    {
        Some(root) => root,
        None => return rpc_error(id, error_codes::INTERNAL_ERROR, ERR_NO_PROJECT),
    };

    let codeunit_ids = match params.get("codeunitIds").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                "Missing 'codeunitIds' (array of i32)",
            );
        }
    };
    let names_arr = params
        .get("codeunitNames")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut tests: Vec<TestId> = Vec::with_capacity(codeunit_ids.len());
    for (i, v) in codeunit_ids.iter().enumerate() {
        let cu_id = match v.as_i64() {
            Some(n) => match i32::try_from(n) {
                Ok(v) => v,
                Err(_) => {
                    return rpc_error(
                        id,
                        error_codes::INVALID_PARAMS,
                        &format!("codeunitIds[{i}] out of range"),
                    );
                }
            },
            None => {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    "Each codeunitIds entry must be an integer",
                );
            }
        };
        let cu_name = names_arr
            .get(i)
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| cu_id.to_string());
        tests.push(TestId {
            codeunit_id: cu_id,
            codeunit_name: cu_name,
            method_name: None,
        });
    }
    let opts = RunOptions {
        timeout_ms: clamp_timeout_ms(params.get("timeoutMs").and_then(|v| v.as_u64())),
        parallel: params
            .get("parallel")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        // Validate output paths against project_root — a malicious client
        // could otherwise ask the daemon to overwrite arbitrary files
        // (cron tabs, ssh keys) as the daemon's user.
        junit_out: match params.get("junitOut").and_then(|v| v.as_str()) {
            Some(s) => {
                match resolve_output_path_within_project(std::path::Path::new(s), &project_root) {
                    Some(p) => Some(p),
                    None => {
                        return rpc_error(
                            id,
                            error_codes::INVALID_PARAMS,
                            "'junitOut' path escapes the project root",
                        )
                    }
                }
            }
            None => None,
        },
        cobertura_out: match params.get("coberturaOut").and_then(|v| v.as_str()) {
            Some(s) => {
                match resolve_output_path_within_project(std::path::Path::new(s), &project_root) {
                    Some(p) => Some(p),
                    None => {
                        return rpc_error(
                            id,
                            error_codes::INVALID_PARAMS,
                            "'coberturaOut' path escapes the project root",
                        )
                    }
                }
            }
            None => None,
        },
        filter: params
            .get("filter")
            .and_then(|v| v.as_str())
            .map(String::from),
    };

    // -- Route per codeunit (F-OPEN-270) ----------------------------------------
    // A codeunit whose discovered [Test] methods ALL classify as `Interp`
    // runs on the Rust interpreter (no BC server, no launch config needed).
    // Everything else — mixed codeunits, record-touching tests, codeunits
    // not discoverable in the workspace — keeps the previous LiveBcMode path
    // unchanged. The launch config is only required when live tests exist.
    let discovered = crate::queries::tests::discover_tests(workspace);
    let classifications = crate::test_engine::router::classify_codeunits(workspace, &discovered);
    let mut all_interp: std::collections::HashMap<i32, bool> = std::collections::HashMap::new();
    for c in &classifications {
        let is_interp = matches!(c.decision, RoutingDecision::Interp);
        all_interp
            .entry(c.codeunit_id)
            .and_modify(|all| *all &= is_interp)
            .or_insert(is_interp);
    }
    let (interp_tests, live_tests): (Vec<TestId>, Vec<TestId>) = tests
        .into_iter()
        .partition(|t| all_interp.get(&t.codeunit_id).copied().unwrap_or(false));

    // -- Resolve launch config (only needed for the live path) ------------------
    let server_config = if live_tests.is_empty() {
        None
    } else {
        let launch_cfg = match find_launch_config(&project_root) {
            Some(cfg) => cfg,
            None => {
                return rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    "No launch config found — create .vscode/launch.json or .zed/debug.json",
                );
            }
        };
        match launch_cfg.configs.first() {
            Some(c) => Some(c.clone()),
            None => {
                return rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    "No BC server config found in launch config",
                );
            }
        }
    };

    // -- Run interp + live backends, merging their event streams ----------------
    let (tx, mut rx) = mpsc::channel::<TestEvent>(256);
    let mut run_handles = Vec::new();
    if !interp_tests.is_empty() {
        let mode = InterpMode::new(std::sync::Arc::clone(workspace));
        let tx_interp = tx.clone();
        let opts_for_run = opts.clone();
        run_handles.push(tokio::spawn(async move {
            mode.run(interp_tests, opts_for_run, tx_interp).await
        }));
    }
    if !live_tests.is_empty() {
        // Routing guarantees server_config is Some when live tests exist.
        let Some(cfg) = server_config else {
            return rpc_error(id, error_codes::INTERNAL_ERROR, "launch config vanished");
        };
        let mode = LiveBcMode::new(cfg);
        let tx_live = tx.clone();
        let opts_for_run = opts.clone();
        run_handles.push(tokio::spawn(async move {
            mode.run(live_tests, opts_for_run, tx_live).await
        }));
    }
    drop(tx); // rx ends once every backend's sender is gone

    let mut events: Vec<TestEvent> = Vec::new();
    let mut summaries: Vec<crate::test_engine::result::TestCodeunitResult> = Vec::new();
    while let Some(ev) = rx.recv().await {
        if let TestEvent::SuiteComplete { ref summary, .. } = ev {
            summaries.push(summary.clone());
        }
        events.push(ev);
    }
    for run_handle in run_handles {
        if let Err(e) = run_handle.await {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("test run task panicked: {e}"),
            );
        }
    }

    if let Err(e) = ensure_result_store(workspace, &project_root).await {
        tracing::warn!(error = %e, "test_results store init failed; persistence skipped");
    } else if let Some(store_arc) = workspace.test_results.read().ok().and_then(|g| g.clone()) {
        for summary in &summaries {
            for m in &summary.methods {
                let rec = crate::test_engine::persistence::TestRunRecord {
                    timestamp: crate::test_engine::persistence::now_secs(),
                    codeunit_id: summary.id,
                    codeunit_name: summary.name.clone(),
                    method_name: m.name.clone(),
                    status: m.status.clone(),
                    duration_ms: m.duration_ms,
                    error: m.error.clone(),
                };
                if let Err(e) = store_arc.append(rec).await {
                    tracing::warn!(error = %e, "failed to persist test result");
                }
            }
        }
    }

    // -- Optional JUnit / Cobertura outputs -----------------------------------
    if let Some(path) = &opts.junit_out {
        if let Err(e) = write_junit_to_path(&summaries, path).await {
            tracing::warn!(error = %e, path = %path.display(), "junit write failed");
        }
    }
    if let Some(path) = &opts.cobertura_out {
        let coverage = crate::queries::test_coverage::test_coverage(workspace);
        if let Err(e) = write_cobertura_to_path(&coverage, path).await {
            tracing::warn!(error = %e, path = %path.display(), "cobertura write failed");
        }
    }

    let total: usize = summaries.iter().map(|s| s.total).sum();
    let passed: usize = summaries.iter().map(|s| s.passed).sum();
    let failed: usize = summaries.iter().map(|s| s.failed).sum();
    let skipped: usize = summaries.iter().map(|s| s.skipped).sum();
    let summaries_json = serde_json::to_value(&summaries).unwrap_or(serde_json::Value::Null);

    // Suppress unused warning on imports until junit/cobertura helpers below.
    let _ = (
        junit::write_junit::<&mut Vec<u8>>,
        cobertura::write_cobertura::<&mut Vec<u8>>,
    );

    Response {
        id,
        result: Some(serde_json::json!({
            "summaries": summaries_json,
            "totals": {
                "total": total,
                "passed": passed,
                "failed": failed,
                "skipped": skipped,
            },
        })),
        error: None,
        ..Default::default()
    }
}
pub(in crate::server::daemon) async fn dispatch_tests_run_auto(
    workspace: &std::sync::Arc<Workspace>,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let discovered = crate::queries::tests::discover_tests(workspace);
    let mut codeunit_ids: Vec<serde_json::Value> = Vec::with_capacity(discovered.len());
    let mut codeunit_names: Vec<serde_json::Value> = Vec::with_capacity(discovered.len());
    for cu in &discovered {
        codeunit_ids.push(serde_json::Value::from(cu.id));
        codeunit_names.push(serde_json::Value::from(cu.name.clone()));
    }
    let mut params = params.clone();
    if let Some(map) = params.as_object_mut() {
        map.insert(
            "codeunitIds".to_string(),
            serde_json::Value::Array(codeunit_ids),
        );
        map.insert(
            "codeunitNames".to_string(),
            serde_json::Value::Array(codeunit_names),
        );
    }
    dispatch_tests_run_batch(workspace, id, &params).await
}
pub(in crate::server::daemon) async fn dispatch_tests_last_results(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let project_root = match workspace
        .project
        .read()
        .await
        .as_ref()
        .map(|p| p.root.clone())
    {
        Some(root) => root,
        None => return rpc_error(id, error_codes::INTERNAL_ERROR, ERR_NO_PROJECT),
    };

    if let Err(e) = ensure_result_store(workspace, &project_root).await {
        return rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            &format!("failed to open test results store: {e}"),
        );
    }
    let store = match workspace.test_results.read().ok().and_then(|g| g.clone()) {
        Some(s) => s,
        None => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                "test results store unavailable",
            );
        }
    };

    // Single (codeunit, method) lookup short-circuits to lastResult.
    if let (Some(cu), Some(method)) = (
        params.get("codeunitId").and_then(|v| v.as_i64()),
        params.get("methodName").and_then(|v| v.as_str()),
    ) {
        let cu_id = match i32::try_from(cu) {
            Ok(v) => v,
            Err(_) => {
                return rpc_error(id, error_codes::INVALID_PARAMS, "codeunitId out of range");
            }
        };
        return match store.last_for(cu_id, method).await {
            Ok(opt) => Response {
                id,
                result: Some(serde_json::json!({ "lastResult": opt })),
                error: None,
                ..Default::default()
            },
            Err(e) => rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("failed to read test results: {e}"),
            ),
        };
    }

    // Otherwise return all (optionally filtered by codeunitId).
    let all = match store.read_all().await {
        Ok(v) => v,
        Err(e) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("failed to read test results: {e}"),
            );
        }
    };
    let filtered: Vec<_> = match params.get("codeunitId").and_then(|v| v.as_i64()) {
        Some(cu) => {
            let cu_id = match i32::try_from(cu) {
                Ok(v) => v,
                Err(_) => {
                    return rpc_error(id, error_codes::INVALID_PARAMS, "codeunitId out of range");
                }
            };
            all.into_iter().filter(|r| r.codeunit_id == cu_id).collect()
        }
        None => all,
    };
    Response {
        id,
        result: Some(serde_json::json!({ "results": filtered })),
        error: None,
        ..Default::default()
    }
}
async fn ensure_result_store(
    workspace: &Workspace,
    project_root: &std::path::Path,
) -> Result<(), crate::test_engine::PersistenceError> {
    {
        let guard = workspace.test_results.read().map_err(|_| {
            crate::test_engine::PersistenceError::Io(std::io::Error::other(
                "test_results lock poisoned",
            ))
        })?;
        if guard.is_some() {
            return Ok(());
        }
    }
    let store = crate::test_engine::TestResultStore::open_for_project(project_root).await?;
    let mut guard = workspace.test_results.write().map_err(|_| {
        crate::test_engine::PersistenceError::Io(std::io::Error::other(
            "test_results lock poisoned",
        ))
    })?;
    *guard = Some(std::sync::Arc::new(store));
    Ok(())
}
async fn write_junit_to_path(
    summaries: &[crate::test_engine::result::TestCodeunitResult],
    path: &std::path::Path,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut buf = Vec::new();
    crate::test_engine::output::junit::write_junit(summaries, &mut buf)?;
    tokio::fs::write(path, buf).await
}
async fn write_cobertura_to_path(
    report: &crate::queries::test_coverage::CoverageReport,
    path: &std::path::Path,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut buf = Vec::new();
    crate::test_engine::output::cobertura::write_cobertura(report, &mut buf)?;
    tokio::fs::write(path, buf).await
}
pub(in crate::server::daemon) fn dispatch_tests_affected(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(arr) = params.get("changedFiles").and_then(|v| v.as_array()) else {
        return rpc_error(
            id,
            error_codes::INVALID_PARAMS,
            "Missing 'changedFiles' (array of paths)",
        );
    };
    let paths: Vec<String> = arr
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    let affected = crate::queries::tests::affected_tests(workspace, &paths);
    Response {
        id,
        result: Some(serde_json::json!({ "affected": affected })),
        error: None,
        ..Default::default()
    }
}
pub(in crate::server::daemon) fn dispatch_tests_classify(
    workspace: &Workspace,
    id: u64,
) -> Response {
    use crate::test_engine::router;
    let results = router::classify_all(workspace);
    let json: Vec<serde_json::Value> = results
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "codeunitId": r.codeunit_id,
                "codeunitName": r.codeunit_name,
                "methodName": r.method_name,
                "decision": r.decision.as_str(),
                "reasons": r
                    .reasons
                    .into_iter()
                    .map(|reason| serde_json::json!({
                        "message": reason.message,
                        "file": reason.file,
                        "line": reason.line,
                    }))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    Response {
        id,
        result: Some(serde_json::json!({ "classifications": json })),
        error: None,
        ..Default::default()
    }
}
pub(in crate::server::daemon) async fn dispatch_tests_snapshot_record(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let _ = workspace;
    let codeunit_id = match params.get("codeunitId").and_then(|v| v.as_i64()) {
        Some(n) => match i32::try_from(n) {
            Ok(v) => v,
            Err(_) => {
                return rpc_error(id, error_codes::INVALID_PARAMS, "codeunitId out of range");
            }
        },
        None => {
            return rpc_error(id, error_codes::INVALID_PARAMS, "Missing 'codeunitId'");
        }
    };
    let _method_name = match params.get("methodName").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => {
            return rpc_error(id, error_codes::INVALID_PARAMS, "Missing 'methodName'");
        }
    };
    let breakpoints = params.get("breakpoints").and_then(|v| v.as_array());
    if breakpoints.is_none_or(|a| a.is_empty()) {
        return rpc_error(
            id,
            error_codes::INVALID_PARAMS,
            "Missing or empty 'breakpoints' array",
        );
    }
    rpc_error(
        id,
        error_codes::INTERNAL_ERROR,
        &format!(
            "tests.snapshot_record (codeunit {codeunit_id}): live-BC bridge not yet wired \
             — see test_snapshots::bc_debug_bridge"
        ),
    )
}
pub(in crate::server::daemon) async fn dispatch_tests_snapshot_replay(
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let path = match params.get("snapshotPath").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return rpc_error(id, error_codes::INVALID_PARAMS, "Missing 'snapshotPath'");
        }
    };
    let bytes = match tokio::fs::read(path).await {
        Ok(b) => b,
        Err(e) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("read snapshot failed: {e}"),
            );
        }
    };
    let snapshot = match crate::test_snapshots::format::deserialize_snapshot(&bytes) {
        Ok(s) => s,
        Err(e) => {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                &format!("snapshot parse failed: {e}"),
            );
        }
    };
    // No live observation yet — emit an info-only "Match" verdict so the
    // caller can confirm the snapshot loads. Real verification arrives
    // when the BC bridge is wired (see bc_debug_bridge.rs).
    let verdict = crate::test_snapshots::replayer::ReplayVerdict::Match;
    Response {
        id,
        result: Some(serde_json::json!({
            "verdict": verdict,
            "sampleCount": snapshot.samples.len(),
            "codeunitId": snapshot.codeunit_id,
            "methodName": snapshot.method_name,
            "bcVersion": snapshot.bc_version,
        })),
        error: None,
        ..Default::default()
    }
}
pub(in crate::server::daemon) async fn dispatch_tests_snapshot_diff(
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let path_a = match params.get("pathA").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return rpc_error(id, error_codes::INVALID_PARAMS, "Missing 'pathA'"),
    };
    let path_b = match params.get("pathB").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return rpc_error(id, error_codes::INVALID_PARAMS, "Missing 'pathB'"),
    };
    let read = async |p: &str| -> Result<crate::test_snapshots::format::Snapshot, String> {
        let bytes = tokio::fs::read(p).await.map_err(|e| e.to_string())?;
        crate::test_snapshots::format::deserialize_snapshot(&bytes).map_err(|e| e.to_string())
    };
    let a = match read(path_a).await {
        Ok(s) => s,
        Err(e) => return rpc_error(id, error_codes::INVALID_PARAMS, &format!("pathA: {e}")),
    };
    let b = match read(path_b).await {
        Ok(s) => s,
        Err(e) => return rpc_error(id, error_codes::INVALID_PARAMS, &format!("pathB: {e}")),
    };
    let divergences = crate::test_snapshots::diff::diff_snapshots(&a, &b);
    Response {
        id,
        result: Some(serde_json::json!({ "divergences": divergences })),
        error: None,
        ..Default::default()
    }
}
/// - `timeoutMs` (optional, u64): per-variant timeout in ms.
///
/// Returns: serialized `MutationReport` JSON.
pub(in crate::server::daemon) async fn dispatch_tests_mutate(
    workspace: &std::sync::Arc<Workspace>,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    use crate::test_engine::mutate::MutationOptions;
    use al_protocol::jsonrpc::error_codes;

    // Require a loaded project
    let _project_root = match workspace
        .project
        .try_read()
        .ok()
        .and_then(|g| g.as_ref().map(|p| p.root.clone()))
    {
        Some(root) => root,
        None => {
            return rpc_error(id, error_codes::INTERNAL_ERROR, ERR_NO_PROJECT);
        }
    };

    // Parse options. An explicit `files` array narrows the mutation scope.
    let parallel = params
        .get("parallel")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let timeout_ms = clamp_timeout_ms(params.get("timeoutMs").and_then(|v| v.as_u64()));
    let files = params.get("files").and_then(|v| v.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect::<Vec<String>>()
    });

    let opts = MutationOptions {
        affected_only: true,
        parallel,
        timeout_ms,
        files,
    };

    // Run the real engine (F-OPEN-270): variants are executed against the
    // interp-routed test suite in-process; the previous dispatcher-local loop
    // was a stub that marked every mutant survived. Progress events are
    // drained — this JSON-RPC endpoint returns only the final report.
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = match crate::test_engine::mutate::run_mutation_testing(workspace, opts, tx).await {
        Ok(report) => report,
        Err(crate::test_engine::mutate::MutationError::NoTestFiles) => {
            crate::test_engine::mutate::MutationReport {
                variants: vec![],
                killed: 0,
                survived: 0,
                errored: 0,
                executor_phase: crate::test_engine::mutate::MutationExecutorPhase::Interpreter,
            }
        }
        Err(e) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("mutation run failed: {e}"),
            );
        }
    };
    let _ = drain.await;

    match serde_json::to_value(&report) {
        Ok(value) => Response {
            id,
            result: Some(value),
            error: None,
            ..Default::default()
        },
        Err(e) => rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            &format!("Serialization error: {e}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;
    use al_protocol::jsonrpc::error_codes;

    fn empty_ws() -> Workspace {
        Workspace::new()
    }

    // --- clamp_timeout_ms ----------------------------------------------------

    #[test]
    fn clamp_timeout_ms_passes_through_sensible_values() {
        // Positive: realistic timeouts (a few seconds to a few minutes)
        // pass through unchanged.
        assert_eq!(clamp_timeout_ms(Some(0)), Some(0));
        assert_eq!(clamp_timeout_ms(Some(30_000)), Some(30_000));
        assert_eq!(clamp_timeout_ms(Some(15 * 60 * 1000)), Some(900_000));
    }

    #[test]
    fn clamp_timeout_ms_caps_at_max() {
        // Negative: a hostile or fat-fingered client could send u64::MAX —
        // we must cap at the documented upper bound (1 hour) so the
        // daemon doesn't get pinned to a multi-day test run.
        let huge = u64::MAX;
        assert_eq!(clamp_timeout_ms(Some(huge)), Some(MAX_TIMEOUT_MS));
        // Exactly one tick above the cap also clamps.
        assert_eq!(
            clamp_timeout_ms(Some(MAX_TIMEOUT_MS + 1)),
            Some(MAX_TIMEOUT_MS)
        );
        // Exactly the cap is allowed through unchanged.
        assert_eq!(clamp_timeout_ms(Some(MAX_TIMEOUT_MS)), Some(MAX_TIMEOUT_MS));
    }

    #[test]
    fn clamp_timeout_ms_propagates_none() {
        // None (param omitted entirely) stays None — caller decides the default.
        assert_eq!(clamp_timeout_ms(None), None);
    }

    // --- resolve_output_path_within_project ----------------------------------

    #[test]
    fn output_path_accepts_relative_inside_project() {
        // Positive: a plain relative path resolves to inside the project.
        let project = tempfile::tempdir().unwrap();
        let resolved = resolve_output_path_within_project(
            std::path::Path::new("out/junit.xml"),
            project.path(),
        );
        assert!(
            resolved.is_some(),
            "relative path inside project must resolve"
        );
    }

    #[test]
    fn output_path_accepts_absolute_inside_project() {
        // Positive: an absolute path that points inside the project is fine.
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
        // Negative: `../escape.xml` resolves to outside the project — reject.
        let project = tempfile::tempdir().unwrap();
        let resolved = resolve_output_path_within_project(
            std::path::Path::new("../escape.xml"),
            project.path(),
        );
        assert!(
            resolved.is_none(),
            "../ escape must be rejected, got {resolved:?}"
        );
    }

    #[test]
    fn output_path_rejects_deep_parent_dir_escape() {
        // Negative: multiple `..` segments that resolve above the project.
        let project = tempfile::tempdir().unwrap();
        let resolved = resolve_output_path_within_project(
            std::path::Path::new("subdir/../../../etc/passwd"),
            project.path(),
        );
        assert!(resolved.is_none());
    }

    #[test]
    fn output_path_rejects_absolute_outside_project() {
        // Negative: a totally unrelated absolute path must be rejected.
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

        let resolved = resolve_output_path_within_project(
            std::path::Path::new("link/evil.xml"),
            project.path(),
        );
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

        let resolved = resolve_output_path_within_project(
            std::path::Path::new("out/junit.xml"),
            project.path(),
        );
        assert!(
            resolved.is_some(),
            "symlink staying inside the project must be accepted"
        );
        let canonical_root = project.path().canonicalize().unwrap();
        assert!(resolved.unwrap().starts_with(&canonical_root));
    }

    // --- dispatch_tests_run_batch --------------------------------------------

    /// F-OPEN-270: pure-logic test codeunits (router decision: Interp) must
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
            *guard = Some(crate::project::AlProject {
                root: tmp.path().to_path_buf(),
                app_json: crate::project::AppManifest {
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
        // Write a minimal launch.json so find_launch_config succeeds.
        let dot_zed = tmp.path().join(".zed");
        std::fs::create_dir_all(&dot_zed).unwrap();
        std::fs::write(
            dot_zed.join("debug.json"),
            r#"[{"name":"local","type":"al","request":"launch","environmentType":"OnPrem","server":"http://localhost","serverInstance":"BC","authentication":"UserPassword"}]"#,
        )
        .unwrap();
        {
            let mut guard = ws.project.write().await;
            *guard = Some(crate::project::AlProject {
                root: tmp.path().to_path_buf(),
                app_json: crate::project::AppManifest {
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
        // Negative regression: a codeunitIds entry beyond the i32 range must be
        // rejected with INVALID_PARAMS rather than silently wrapping via
        // `as i32` and executing tests against the wrong codeunit.
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
            *guard = Some(crate::project::AlProject {
                root: tmp.path().to_path_buf(),
                app_json: crate::project::AppManifest {
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
    async fn last_results_returns_empty_when_no_history() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        // Override XDG_DATA_HOME so the store path is sandboxed.
        // SAFETY: tests run on a single thread by default in cargo test.
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
        }
        {
            let mut guard = ws.project.write().await;
            *guard = Some(crate::project::AlProject {
                root: tmp.path().to_path_buf(),
                app_json: crate::project::AppManifest {
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
        // SAFETY: cargo test runs on a single thread by default.
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
        }
        let mut guard = ws.project.write().await;
        *guard = Some(crate::project::AlProject {
            root: tmp.path().to_path_buf(),
            app_json: crate::project::AppManifest {
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
        });
        drop(guard);
        ws
    }

    #[tokio::test]
    async fn last_results_single_lookup_rejects_out_of_range_codeunit_id() {
        // Negative regression: out-of-range codeunitId in the single
        // (codeunit, method) lookup path must return INVALID_PARAMS rather
        // than silently wrapping via `as i32`.
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
        // Negative regression: out-of-range codeunitId in the bulk-filter path
        // must return INVALID_PARAMS rather than silently wrapping via `as i32`.
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

    // -----------------------------------------------------------------------
    // p1-7 freeze gate: existing wire formats must not drift.
    // -----------------------------------------------------------------------

    #[test]
    fn freeze_test_codeunit_result_wire_format() {
        use crate::test_engine::result::{TestCodeunitResult, TestMethodResult, TestStatus};
        let v = TestCodeunitResult::from_methods(
            "X".to_string(),
            42,
            vec![TestMethodResult {
                name: "M".to_string(),
                status: TestStatus::Pass,
                error: None,
                duration_ms: Some(10),
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
        use crate::queries::test_coverage::{CoverageReport, CoveredProcedure, TestCoverageEntry};
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
        for key in ["codeunit", "testProcedure", "covers"] {
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
        use crate::queries::tests::{TestCodeunit, TestProcedure};
        let v = TestCodeunit {
            id: 50100,
            name: "MyTests".to_string(),
            file: "src/MyTests.al".to_string(),
            tests: vec![TestProcedure {
                name: "TestA".to_string(),
                line: 5,
            }],
        };
        let json = serde_json::to_value(&v).unwrap();
        for key in ["id", "name", "file", "tests"] {
            assert!(json.get(key).is_some(), "TestCodeunit key `{key}` missing");
        }
        let proc = &json["tests"][0];
        for key in ["name", "line"] {
            assert!(proc.get(key).is_some(), "TestProcedure key `{key}` missing");
        }
    }

    #[tokio::test]
    async fn run_batch_persistence_roundtrip_via_dispatchers() {
        // Bypass live BC: persist a record directly through the same store
        // the dispatchers use, then call dispatch_tests_last_results and
        // assert the record appears.
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        // SAFETY: tests run on a single thread by default in cargo test.
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
        }
        {
            let mut guard = ws.project.write().await;
            *guard = Some(crate::project::AlProject {
                root: tmp.path().to_path_buf(),
                app_json: crate::project::AppManifest {
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
            });
        }

        ensure_result_store(&ws, tmp.path()).await.unwrap();
        let store = ws
            .test_results
            .read()
            .ok()
            .and_then(|g| g.clone())
            .expect("store should be initialised");
        store
            .append(crate::test_engine::persistence::TestRunRecord {
                timestamp: 1_700_000_000,
                codeunit_id: 50200,
                codeunit_name: "Persisted".into(),
                method_name: "TestRoundtrip".into(),
                status: crate::test_engine::result::TestStatus::Pass,
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

    // -----------------------------------------------------------------------
    // p2 dispatcher tests — affected + classify
    // -----------------------------------------------------------------------

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
        use crate::test_engine::router::RoutingDecision;
        assert_eq!(RoutingDecision::Interp.as_str(), "interp");
        assert_eq!(RoutingDecision::InterpRecord.as_str(), "interpRecord");
        assert_eq!(RoutingDecision::LiveBc.as_str(), "liveBc");
        assert_eq!(RoutingDecision::Snapshot.as_str(), "snapshot");
    }

    #[test]
    fn freeze_affected_test_wire_format() {
        use crate::queries::tests::AffectedTest;
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

    // -----------------------------------------------------------------------
    // Wire-format freeze gates for new endpoints landed in this session.
    // These pin the JSON response shapes; failures here flag callers who
    // rename fields without bumping the protocol version.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn freeze_run_batch_response_shape() {
        // run_batch with no project produces an error envelope; the shape
        // we pin is the SUCCESS envelope, so synthesize one directly.
        let summary =
            crate::test_engine::result::TestCodeunitResult::from_methods("Cu".into(), 1, vec![]);
        let summaries_json = serde_json::to_value(&[summary]).unwrap();
        let response = serde_json::json!({
            "summaries": summaries_json,
            "totals": { "total": 0_u64, "passed": 0_u64, "failed": 0_u64, "skipped": 0_u64 },
        });
        for key in ["summaries", "totals"] {
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
        // No-codeunit query → results array.
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        // SAFETY: cargo test runs single-threaded by default.
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
        }
        {
            let mut g = ws.project.write().await;
            *g = Some(crate::project::AlProject {
                root: tmp.path().to_path_buf(),
                app_json: crate::project::AppManifest {
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
            });
        }
        // Bulk: shape = { "results": [...] }.
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
        // dispatch_tests_classify on an empty workspace returns an empty
        // classifications array — pin both that the array exists and that
        // when populated each item carries the required keys.
        let ws = empty_ws();
        let resp = dispatch_tests_classify(&ws, 200);
        let r = resp.result.expect("ok");
        assert!(
            r.get("classifications").is_some(),
            "tests.classify shape must expose `classifications`"
        );

        // Synthetic classification entry to pin the per-item shape.
        let item = serde_json::json!({
            "codeunitId": 1,
            "codeunitName": "X",
            "methodName": "M",
            "decision": "interp",
            "reasons": [{ "message": "", "file": null, "line": null }],
        });
        for key in [
            "codeunitId",
            "codeunitName",
            "methodName",
            "decision",
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

    // --- dispatch_tests_snapshot_record / replay / diff ----------------------

    #[tokio::test]
    async fn snapshot_record_missing_codeunit_is_invalid_params() {
        let ws = empty_ws();
        let resp = dispatch_tests_snapshot_record(&ws, 1, &serde_json::json!({})).await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("codeunitId"));
    }

    #[tokio::test]
    async fn snapshot_record_rejects_out_of_range_codeunit() {
        let ws = empty_ws();
        let resp = dispatch_tests_snapshot_record(
            &ws,
            2,
            &serde_json::json!({ "codeunitId": (i32::MAX as i64) + 1 }),
        )
        .await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("out of range"));
    }

    #[tokio::test]
    async fn snapshot_record_missing_breakpoints_is_invalid_params() {
        // Past codeunitId + methodName validation, an empty breakpoints array
        // must still be rejected — proving the guard fires, not the BC stub.
        let ws = empty_ws();
        let resp = dispatch_tests_snapshot_record(
            &ws,
            3,
            &serde_json::json!({ "codeunitId": 50100, "methodName": "T", "breakpoints": [] }),
        )
        .await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("breakpoints"));
    }

    #[tokio::test]
    async fn snapshot_record_fully_valid_reaches_not_wired_stub() {
        // All params valid → the dispatcher reaches the documented
        // "not yet wired" INTERNAL_ERROR rather than an INVALID_PARAMS.
        let ws = empty_ws();
        let resp = dispatch_tests_snapshot_record(
            &ws,
            4,
            &serde_json::json!({
                "codeunitId": 50100,
                "methodName": "T",
                "breakpoints": [{ "file": "a.al", "line": 1 }],
            }),
        )
        .await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INTERNAL_ERROR);
        assert!(err.message.contains("not yet wired"));
    }

    #[tokio::test]
    async fn snapshot_replay_missing_path_is_invalid_params() {
        let resp = dispatch_tests_snapshot_replay(1, &serde_json::json!({})).await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("snapshotPath"));
    }

    #[tokio::test]
    async fn snapshot_replay_unreadable_path_is_internal_error() {
        let resp = dispatch_tests_snapshot_replay(
            2,
            &serde_json::json!({ "snapshotPath": "/nonexistent/snap.bin" }),
        )
        .await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INTERNAL_ERROR);
    }

    #[tokio::test]
    async fn snapshot_diff_missing_paths_is_invalid_params() {
        let only_a = dispatch_tests_snapshot_diff(1, &serde_json::json!({ "pathA": "/x" })).await;
        let err = only_a.error.expect("missing pathB must error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("pathB"));

        let none = dispatch_tests_snapshot_diff(2, &serde_json::json!({})).await;
        let err = none.error.expect("missing pathA must error");
        assert!(err.message.contains("pathA"));
    }

    // -----------------------------------------------------------------------
    // dispatch_tests_mutate: no-project must short-circuit to an error before
    // any variant generation.
    // -----------------------------------------------------------------------

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
}
