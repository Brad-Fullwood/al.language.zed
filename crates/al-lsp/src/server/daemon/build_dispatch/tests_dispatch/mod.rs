//! Test-runner and coverage dispatchers.

use super::super::containment::resolve_output_path_within_project;
use super::super::{optional_bool_param, optional_bounded_usize_param, rpc_error};
use super::ERR_NO_PROJECT;
use al_protocol::jsonrpc::{error_codes, Response};
use al_workspace::Workspace;
use std::path::PathBuf;

const MAX_TIMEOUT_MS: u64 = 60 * 60 * 1000;

fn optional_timeout_ms(params: &serde_json::Value) -> Result<Option<u64>, String> {
    if params.get("timeoutMs").is_none() {
        return Ok(None);
    }
    optional_bounded_usize_param(params, "timeoutMs", 0, MAX_TIMEOUT_MS as usize)
        .map(|value| Some(value as u64))
}

fn optional_non_empty_string<'a>(
    params: &'a serde_json::Value,
    key: &str,
) -> Result<Option<&'a str>, String> {
    match params.get(key) {
        None => Ok(None),
        Some(value) => {
            let value = value
                .as_str()
                .ok_or_else(|| format!("'{key}' must be a string when supplied"))?
                .trim();
            if value.is_empty() {
                Err(format!("'{key}' must not be empty"))
            } else {
                Ok(Some(value))
            }
        }
    }
}

fn optional_array<'a>(
    params: &'a serde_json::Value,
    key: &str,
) -> Result<Option<&'a Vec<serde_json::Value>>, String> {
    match params.get(key) {
        None => Ok(None),
        Some(value) => value
            .as_array()
            .map(Some)
            .ok_or_else(|| format!("'{key}' must be an array when supplied")),
    }
}

/// Resolve a mutation-file allowlist against the loaded project and translate
/// every entry to the exact path spelling held by `FileIndex`.
///
/// The CLI accepts workspace-relative paths, while the index normally contains
/// absolute paths. Comparing those strings directly made a valid selection
/// look empty and produced the misleading "no discoverable tests" error. This
/// boundary is also the right place to reject missing, out-of-project, and
/// non-indexed files before the mutation engine starts an expensive run.
fn resolve_mutation_file_allowlist(
    workspace: &Workspace,
    project_root: &std::path::Path,
    files: Vec<String>,
) -> Result<Vec<String>, String> {
    let canonical_root = project_root
        .canonicalize()
        .map_err(|error| format!("resolve loaded project root failed: {error}"))?;
    let mut resolved = Vec::with_capacity(files.len());
    let mut seen = std::collections::HashSet::with_capacity(files.len());

    for (index, file) in files.into_iter().enumerate() {
        let requested = std::path::Path::new(&file);
        let candidate = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            canonical_root.join(requested)
        };
        let canonical = candidate.canonicalize().map_err(|error| {
            format!(
                "files[{index}] '{}' cannot be resolved: {error}",
                candidate.display()
            )
        })?;
        if !canonical.is_file() {
            return Err(format!(
                "files[{index}] '{}' is not a regular file",
                canonical.display()
            ));
        }
        if !canonical.starts_with(&canonical_root) {
            return Err(format!(
                "files[{index}] '{}' is outside the loaded project",
                canonical.display()
            ));
        }
        if !seen.insert(canonical.clone()) {
            return Err(format!(
                "files[{index}] resolves to the same file as an earlier entry: '{}'",
                canonical.display()
            ));
        }

        let indexed_path = workspace.file_index.files.iter().find_map(|entry| {
            let indexed = entry.key();
            if indexed == &canonical
                || indexed
                    .canonicalize()
                    .ok()
                    .is_some_and(|path| path == canonical)
            {
                Some(indexed.to_string_lossy().into_owned())
            } else {
                None
            }
        });
        let Some(indexed_path) = indexed_path else {
            return Err(format!(
                "files[{index}] '{}' is not an indexed AL workspace file",
                canonical.display()
            ));
        };
        resolved.push(indexed_path);
    }

    Ok(resolved)
}

/// The first reason a test was sent to live Business Central, skipping the
/// "calls supported ..." notes the router records beside the reason that
/// disqualified it.
/// Why the codeunits sent to live BC went there. Only those codeunits: the
/// first live classification of the whole workspace named another
/// codeunit's reason (`test-run` of a codeunit using a workspace table said
/// it needed BC for the Customer table).
fn live_reason(
    classifications: &[al_test::router::ClassifyResult],
    live_codeunits: &std::collections::BTreeSet<i32>,
) -> Option<String> {
    let live = classifications.iter().find(|c| {
        c.decision == al_test::router::RoutingDecision::LiveBc
            && live_codeunits.contains(&c.codeunit_id)
    })?;
    live.reasons
        .iter()
        .find(|reason| !reason.message.contains("calls supported"))
        .or_else(|| live.reasons.first())
        .map(|reason| reason.message.clone())
}

fn test_routing_details(
    classifications: &[al_test::router::ClassifyResult],
    requested: &[al_test::session::TestId],
) -> Vec<serde_json::Value> {
    use al_test::router::RoutingDecision;
    use std::collections::HashMap;

    let mut codeunit_route: HashMap<i32, RoutingDecision> = HashMap::new();
    for classification in classifications {
        codeunit_route
            .entry(classification.codeunit_id)
            .and_modify(|decision| {
                *decision = match (*decision, classification.decision) {
                    (RoutingDecision::LiveBc, _) | (_, RoutingDecision::LiveBc) => {
                        RoutingDecision::LiveBc
                    }
                    (RoutingDecision::InterpRecord, _) | (_, RoutingDecision::InterpRecord) => {
                        RoutingDecision::InterpRecord
                    }
                    _ => RoutingDecision::Interp,
                };
            })
            .or_insert(classification.decision);
    }

    let mut details = Vec::new();
    for test in requested {
        let matching = classifications.iter().filter(|classification| {
            classification.codeunit_id == test.codeunit_id
                && test
                    .method_name
                    .as_ref()
                    .is_none_or(|method| classification.method_name.eq_ignore_ascii_case(method))
        });
        let mut matched = false;
        for classification in matching {
            matched = true;
            let actual = codeunit_route
                .get(&classification.codeunit_id)
                .copied()
                .unwrap_or(RoutingDecision::LiveBc);
            let mut reasons = classification
                .reasons
                .iter()
                .map(|reason| {
                    serde_json::json!({
                        "message": reason.message,
                        "file": reason.file,
                        "line": reason.line,
                    })
                })
                .collect::<Vec<_>>();
            if reasons.is_empty() {
                reasons.push(serde_json::json!({
                    "message": "no unsupported runtime behavior was detected",
                    "file": serde_json::Value::Null,
                    "line": serde_json::Value::Null,
                }));
            }
            if actual != classification.decision {
                let message = match actual {
                    RoutingDecision::InterpRecord => {
                        "this codeunit also contains a record-backed test, so its shared lifecycle runs on the local record runtime"
                    }
                    RoutingDecision::LiveBc => {
                        "this codeunit also contains a test that requires live BC; codeunit lifecycle and shared state keep every method on one backend"
                    }
                    RoutingDecision::Interp => {
                        "the complete codeunit is supported by the local interpreter"
                    }
                };
                reasons.push(serde_json::json!({
                    "message": message,
                    "file": serde_json::Value::Null,
                    "line": serde_json::Value::Null,
                }));
            }
            details.push(serde_json::json!({
                "codeunitId": classification.codeunit_id,
                "codeunitName": classification.codeunit_name,
                "methodName": classification.method_name,
                "classifiedDecision": classification.decision.as_str(),
                "decision": actual.as_str(),
                "runsLocally": actual.runs_locally(),
                "execution": actual.execution_note(),
                "reasons": reasons,
            }));
        }
        if !matched {
            details.push(serde_json::json!({
                "codeunitId": test.codeunit_id,
                "codeunitName": test.codeunit_name,
                "methodName": test.method_name,
                "classifiedDecision": serde_json::Value::Null,
                "decision": RoutingDecision::LiveBc.as_str(),
                "runsLocally": false,
                "execution": RoutingDecision::LiveBc.execution_note(),
                "reasons": [{
                    "message": "the requested test was not present in the workspace test index, so the router cannot prove local execution is safe",
                    "file": serde_json::Value::Null,
                    "line": serde_json::Value::Null,
                }],
            }));
        }
    }
    details
}

pub(in crate::server::daemon) fn dispatch_tests_discover(
    workspace: &Workspace,
    id: u64,
) -> Response {
    let tests = match al_analysis::queries::tests::discover_tests(workspace) {
        Ok(tests) => tests,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("test discovery failed: {error}"),
            );
        }
    };
    let value = match serde_json::to_value(&tests) {
        Ok(value) => value,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("test discovery response serialization failed: {error}"),
            );
        }
    };
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
    let report = match al_analysis::queries::test_coverage::test_coverage(workspace) {
        Ok(report) => report,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("test coverage analysis failed: {error}"),
            );
        }
    };
    let value = match serde_json::to_value(&report) {
        Ok(value) => value,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("test coverage response serialization failed: {error}"),
            );
        }
    };
    Response {
        id,
        result: Some(value),
        error: None,
        ..Default::default()
    }
}
/// Execute tests via BC REST API + Return results as diagnostics.
///
/// Params:
/// - `codeunit` (i64): codeunit ID to run. Required.
/// - `codeunitName` (str): display name for the result. Defaults to the ID as a string.
/// - `method` (str, optional): run only this test method.
///
/// The normal router selects a local interpreter tier when possible. Only a
/// LiveBc decision reads launch config from the project root
/// (`.vscode/launch.json` or `.zed/debug.json`); the first config is used unless
/// `config` names one explicitly.
///
/// Response includes:
/// - `result`: `TestCodeunitResult` JSON
/// - `diagnostics`: array of `TestDiagnostic` for failed/skipped tests
pub(in crate::server::daemon) async fn dispatch_tests_run(
    workspace: &std::sync::Arc<Workspace>,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    use al_analysis::queries::test_diagnostics::results_to_diagnostics;

    let codeunit_id = match params.get("codeunit").and_then(|v| v.as_i64()) {
        Some(n) => match i32::try_from(n) {
            Ok(v) => v,
            Err(_) => {
                return rpc_error(id, error_codes::INVALID_PARAMS, "codeunit ID out of range");
            }
        },
        _ => {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                "Missing 'codeunit' parameter (i64 codeunit ID)",
            );
        }
    };
    if codeunit_id <= 0 {
        return rpc_error(
            id,
            error_codes::INVALID_PARAMS,
            "'codeunit' must be a positive AL object ID",
        );
    }
    let codeunit_name = match optional_non_empty_string(params, "codeunitName") {
        Ok(Some(name)) => name.to_string(),
        Ok(None) => codeunit_id.to_string(),
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let method = match optional_non_empty_string(params, "method") {
        Ok(method) => method.map(str::to_string),
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let config_name = match optional_non_empty_string(params, "config") {
        Ok(config) => config,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };

    // Reuse the batch orchestrator so single-codeunit runs obey exactly the
    // same Interp / InterpRecord / LiveBc routing contract. This also keeps
    // persistence and timeout behavior consistent across CLI, TUI, and MCP.
    let mut batch_params = serde_json::json!({
        "codeunitIds": [codeunit_id],
        "codeunitNames": [codeunit_name],
    });
    if let Some(method) = method {
        batch_params["methodNames"] = serde_json::json!([method]);
    }
    if let Some(config_name) = config_name {
        batch_params["config"] = serde_json::Value::String(config_name.to_string());
    }
    let batch_response = dispatch_tests_run_batch(workspace, id, &batch_params).await;
    if batch_response.error.is_some() {
        return batch_response;
    }
    let result = match batch_response
        .result
        .as_ref()
        .and_then(|value| value.get("summaries"))
        .and_then(|value| value.as_array())
        .and_then(|summaries| summaries.first())
        .cloned()
        .and_then(|value| serde_json::from_value::<al_test::result::TestCodeunitResult>(value).ok())
    {
        Some(result) => result,
        None => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                "Test run completed without a codeunit summary",
            );
        }
    };

    let diagnostics = match results_to_diagnostics(std::slice::from_ref(&result), workspace) {
        Ok(diagnostics) => diagnostics,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("test diagnostic mapping failed: {error}"),
            );
        }
    };

    let result_json = match serde_json::to_value(&result) {
        Ok(value) => value,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("test result serialization failed: {error}"),
            );
        }
    };
    let diag_json = match serde_json::to_value(&diagnostics) {
        Ok(value) => value,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("test diagnostics serialization failed: {error}"),
            );
        }
    };

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
/// `tests.run_batch` — run multiple codeunits, optionally in parallel,
/// optionally writing JUnit/Cobertura output to disk.
///
/// Params:
/// - `codeunitIds`: `[i32]` (required)
/// - `codeunitNames`: `[str]` (parallel-indexed; falls back to ID-as-string)
/// - `methodNames`: `[str|null]` (parallel-indexed; omitted/null runs all methods)
/// - `config`: str (named live-BC launch config; defaults to first)
/// - `parallel`: bool (default false)
/// - `timeoutMs`: u64 (default 30_000)
/// - `junitOut`: str (path to write JUnit XML)
/// - `coberturaOut`: str (path to write Cobertura XML)
/// - `filter`: str (case-insensitive method glob; `*` wildcard)
/// - `coverage`: bool (default false) — collect *dynamic* executed-line
///   coverage on interp-routed tests. Adds a `coverage` object to the result
///   (per-file executed lines + branch decisions) and, when `coberturaOut` is
///   set, writes a dynamic-mode Cobertura doc instead of the static one.
pub(in crate::server::daemon) async fn dispatch_tests_run_batch(
    workspace: &std::sync::Arc<Workspace>,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    use al_bc::launch::find_launch_config;
    use al_test::backends::interp::InterpMode;
    use al_test::backends::live_bc::LiveBcMode;
    use al_test::output::{cobertura, junit};
    use al_test::router::RoutingDecision;
    use al_test::session::{RunOptions, TestEvent, TestId, TestSession};
    use tokio::sync::mpsc;

    let codeunit_ids = match optional_array(params, "codeunitIds") {
        Ok(Some(codeunit_ids)) => codeunit_ids,
        Ok(None) => {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                "Missing 'codeunitIds' (array of i32)",
            );
        }
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let names_arr = match optional_array(params, "codeunitNames") {
        Ok(names) => names,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let methods_arr = match optional_array(params, "methodNames") {
        Ok(methods) => methods,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    if names_arr.is_some_and(|names| names.len() != codeunit_ids.len()) {
        return rpc_error(
            id,
            error_codes::INVALID_PARAMS,
            "'codeunitNames' must contain one entry for every codeunit ID",
        );
    }
    if methods_arr.is_some_and(|methods| methods.len() != codeunit_ids.len()) {
        return rpc_error(
            id,
            error_codes::INVALID_PARAMS,
            "'methodNames' must contain one string or null for every codeunit ID",
        );
    }
    let parallel = match optional_bool_param(params, "parallel", false) {
        Ok(parallel) => parallel,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let coverage = match optional_bool_param(params, "coverage", false) {
        Ok(coverage) => coverage,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let timeout_ms = match optional_timeout_ms(params) {
        Ok(timeout) => timeout,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let config_name = match optional_non_empty_string(params, "config") {
        Ok(config) => config,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let junit_out = match optional_non_empty_string(params, "junitOut") {
        Ok(path) => path,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let cobertura_out = match optional_non_empty_string(params, "coberturaOut") {
        Ok(path) => path,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let filter = match optional_non_empty_string(params, "filter") {
        Ok(filter) => filter.map(str::to_string),
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };

    let mut tests: Vec<TestId> = Vec::with_capacity(codeunit_ids.len());
    let mut requested = std::collections::HashSet::new();
    for (index, value) in codeunit_ids.iter().enumerate() {
        let codeunit_id = match value.as_i64().and_then(|value| i32::try_from(value).ok()) {
            Some(codeunit_id) if codeunit_id > 0 => codeunit_id,
            _ => {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    &format!(
                        "codeunitIds[{index}] is out of range (must be a positive i32 AL object ID)"
                    ),
                );
            }
        };
        let codeunit_name = match names_arr.and_then(|names| names.get(index)) {
            None => codeunit_id.to_string(),
            Some(value) => match value.as_str().filter(|name| !name.trim().is_empty()) {
                Some(name) => name.to_string(),
                None => {
                    return rpc_error(
                        id,
                        error_codes::INVALID_PARAMS,
                        &format!("codeunitNames[{index}] must be a non-empty string"),
                    );
                }
            },
        };
        let method_name = match methods_arr.and_then(|methods| methods.get(index)) {
            None | Some(serde_json::Value::Null) => None,
            Some(value) => match value.as_str().filter(|method| !method.trim().is_empty()) {
                Some(method) => Some(method.to_string()),
                None => {
                    return rpc_error(
                        id,
                        error_codes::INVALID_PARAMS,
                        &format!("methodNames[{index}] must be a non-empty string or null"),
                    );
                }
            },
        };
        let request_key = (
            codeunit_id,
            method_name
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase(),
        );
        if !requested.insert(request_key) {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                &format!("duplicate test request at codeunitIds[{index}]"),
            );
        }
        tests.push(TestId {
            codeunit_id,
            codeunit_name,
            method_name,
        });
    }

    let project_root = match workspace
        .project
        .read()
        .await
        .as_ref()
        .map(|project| project.root.clone())
    {
        Some(root) => root,
        None => return rpc_error(id, error_codes::INTERNAL_ERROR, ERR_NO_PROJECT),
    };
    let opts = RunOptions {
        timeout_ms,
        parallel,
        // No RPC parameter yet; the backend default keeps the live-BC fan-out
        // inside a typical on-prem concurrent-session limit.
        max_parallel: None,
        // Validate output paths against project_root — a malicious client
        // could otherwise ask the daemon to overwrite arbitrary files
        // (cron tabs, ssh keys) as the daemon's user.
        junit_out: match junit_out {
            Some(s) => {
                match resolve_output_path_within_project(std::path::Path::new(s), &project_root) {
                    Some(p) => Some(p),
                    None => {
                        return rpc_error(
                            id,
                            error_codes::INVALID_PARAMS,
                            "'junitOut' path escapes the project root",
                        );
                    }
                }
            }
            None => None,
        },
        cobertura_out: match cobertura_out {
            Some(s) => {
                match resolve_output_path_within_project(std::path::Path::new(s), &project_root) {
                    Some(p) => Some(p),
                    None => {
                        return rpc_error(
                            id,
                            error_codes::INVALID_PARAMS,
                            "'coberturaOut' path escapes the project root",
                        );
                    }
                }
            }
            None => None,
        },
        filter,
        // Opt-in dynamic (executed-line) coverage. When set, interp-routed
        // tests run with a collector and we surface the per-file executed lines in
        // the RPC result + emit a dynamic-mode Cobertura doc to `coberturaOut`.
        coverage,
    };

    let discovered = match al_analysis::queries::tests::discover_tests(workspace) {
        Ok(discovered) => discovered,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("test discovery failed: {error}"),
            );
        }
    };
    if let Some(pattern) = opts.filter.as_deref() {
        let mut filtered = Vec::new();
        for test in tests {
            match test.method_name.as_deref() {
                Some(method) if al_test::session::method_name_matches(method, pattern) => {
                    filtered.push(test);
                }
                Some(_) => {}
                None => {
                    if let Some(codeunit) = discovered
                        .iter()
                        .find(|codeunit| codeunit.id == test.codeunit_id)
                    {
                        filtered.extend(
                            codeunit
                                .tests
                                .iter()
                                .filter(|method| {
                                    al_test::session::method_name_matches(&method.name, pattern)
                                })
                                .map(|method| al_test::session::TestId {
                                    codeunit_id: test.codeunit_id,
                                    codeunit_name: test.codeunit_name.clone(),
                                    method_name: Some(method.name.clone()),
                                }),
                        );
                    } else {
                        // Preserve conservative behavior for a caller-supplied
                        // codeunit absent from workspace discovery; the live
                        // backend can list and filter its methods.
                        filtered.push(test);
                    }
                }
            }
        }
        tests = filtered;
        if tests.is_empty() && !codeunit_ids.is_empty() {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                &format!("test filter '{pattern}' matched no requested test methods"),
            );
        }
    }
    let expected_summary_ids = tests
        .iter()
        .map(|test| test.codeunit_id)
        .collect::<std::collections::BTreeSet<_>>();
    let classifications = match al_test::router::classify_codeunits(workspace, &discovered) {
        Ok(classifications) => classifications,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("test routing analysis failed: {error}"),
            );
        }
    };
    let routing = test_routing_details(&classifications, &tests);
    // A codeunit is executed locally only when every discovered [Test] method
    // fits one of the native capability tiers. Mixed pure/record codeunits use
    // the record-enabled interpreter; any LiveBc method keeps the
    // whole codeunit on the authoritative server so codeunit-level lifecycle
    // and shared state cannot be split across backends.
    let mut local_capability: std::collections::HashMap<i32, (bool, bool)> =
        std::collections::HashMap::new();
    for c in &classifications {
        let (is_local, needs_records) = match c.decision {
            RoutingDecision::Interp => (true, false),
            RoutingDecision::InterpRecord => (true, true),
            RoutingDecision::LiveBc => (false, false),
        };
        local_capability
            .entry(c.codeunit_id)
            .and_modify(|state| {
                state.0 &= is_local;
                state.1 |= needs_records;
            })
            .or_insert((is_local, needs_records));
    }
    let (local_tests, live_tests): (Vec<TestId>, Vec<TestId>) = tests.into_iter().partition(|t| {
        local_capability
            .get(&t.codeunit_id)
            .is_some_and(|(is_local, _)| *is_local)
    });
    let (record_tests, interp_tests): (Vec<TestId>, Vec<TestId>) =
        local_tests.into_iter().partition(|t| {
            local_capability
                .get(&t.codeunit_id)
                .is_some_and(|(_, needs_records)| *needs_records)
        });

    let server_config = if live_tests.is_empty() {
        None
    } else {
        let launch_cfg = match find_launch_config(&project_root) {
            Ok(Some(cfg)) => cfg,
            Ok(None) => {
                return rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    "No launch config found — create .vscode/launch.json or .zed/debug.json",
                );
            }
            Err(error) => {
                return rpc_error(id, error_codes::INVALID_PARAMS, &error.to_string());
            }
        };
        let selected = match config_name {
            Some(name) => launch_cfg
                .configs
                .iter()
                .find(|config| config.name.eq_ignore_ascii_case(name)),
            None => launch_cfg.configs.first(),
        };
        match selected {
            Some(c) => Some(c.clone()),
            None => {
                let message = match config_name {
                    Some(name) => {
                        let known = launch_cfg
                            .configs
                            .iter()
                            .map(|config| config.name.as_str())
                            .collect::<Vec<_>>();
                        format!("BC server config '{name}' not found; known configs: {known:?}")
                    }
                    None => "No BC server config found in launch config".to_string(),
                };
                return rpc_error(
                    id,
                    if config_name.is_some() {
                        error_codes::INVALID_PARAMS
                    } else {
                        error_codes::INTERNAL_ERROR
                    },
                    &message,
                );
            }
        }
    };

    let (tx, mut rx) = mpsc::channel::<TestEvent>(256);
    let mut run_handles = Vec::new();
    // When dynamic coverage is requested, hold a handle to the interp
    // backend (behind Arc — `run` takes &self) so we can read its aggregated
    // DynamicCoverageReport once the run completes.
    let mut interp_modes_for_report: Vec<std::sync::Arc<InterpMode>> = Vec::new();
    if !interp_tests.is_empty() {
        let mode = std::sync::Arc::new(if opts.coverage {
            InterpMode::with_coverage(std::sync::Arc::clone(workspace))
        } else {
            InterpMode::new(std::sync::Arc::clone(workspace))
        });
        interp_modes_for_report.push(std::sync::Arc::clone(&mode));
        let tx_interp = tx.clone();
        let opts_for_run = opts.clone();
        let mode_for_run = std::sync::Arc::clone(&mode);
        run_handles.push(tokio::spawn(async move {
            mode_for_run
                .run(interp_tests, opts_for_run, tx_interp)
                .await
        }));
    }
    if !record_tests.is_empty() {
        let mode = std::sync::Arc::new(if opts.coverage {
            InterpMode::with_records_and_coverage(std::sync::Arc::clone(workspace))
        } else {
            InterpMode::with_records(std::sync::Arc::clone(workspace))
        });
        interp_modes_for_report.push(std::sync::Arc::clone(&mode));
        let tx_interp = tx.clone();
        let opts_for_run = opts.clone();
        let mode_for_run = std::sync::Arc::clone(&mode);
        run_handles.push(tokio::spawn(async move {
            mode_for_run
                .run(record_tests, opts_for_run, tx_interp)
                .await
        }));
    }
    let live_codeunit_ids = live_tests
        .iter()
        .map(|test| test.codeunit_id)
        .collect::<std::collections::BTreeSet<_>>();
    let live_codeunits = live_codeunit_ids.len();
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
    let mut summaries: Vec<al_test::result::TestCodeunitResult> = Vec::new();
    while let Some(ev) = rx.recv().await {
        if let TestEvent::SuiteComplete { ref summary, .. } = ev {
            summaries.push(summary.clone());
        }
        events.push(ev);
    }
    for run_handle in run_handles {
        match run_handle.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                return rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    &format!("test backend failed: {error}"),
                );
            }
            Err(error) => {
                return rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    &format!("test run task panicked: {error}"),
                );
            }
        }
    }
    let backend_errors = events
        .iter()
        .filter_map(|event| match event {
            TestEvent::Error { message } => Some(message.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if !backend_errors.is_empty() {
        // "Missing credentials" alone left the caller to guess why tests it
        // expected to run locally needed a server at all.
        let live_note = live_reason(&classifications, &live_codeunit_ids)
            .map(|reason| {
                format!(
                    ". The tests in {live_codeunits} codeunit(s) were routed to live Business \
                     Central because the code {reason}; `al-explorer test-classify` shows the \
                     route of each test"
                )
            })
            .unwrap_or_default();
        return rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            &format!(
                "test backends reported errors: {}{live_note}",
                backend_errors.join("; ")
            ),
        );
    }
    let actual_summary_ids = summaries
        .iter()
        .map(|summary| summary.id)
        .collect::<std::collections::BTreeSet<_>>();
    let missing_summaries = expected_summary_ids
        .difference(&actual_summary_ids)
        .copied()
        .collect::<Vec<_>>();
    if !missing_summaries.is_empty() {
        return rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            &format!(
                "test backends completed without summaries for codeunits {missing_summaries:?}"
            ),
        );
    }
    let unexpected_summaries = actual_summary_ids
        .difference(&expected_summary_ids)
        .copied()
        .collect::<Vec<_>>();
    if !unexpected_summaries.is_empty() || actual_summary_ids.len() != summaries.len() {
        return rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            &format!(
                "test backends returned duplicate or unexpected summaries: {unexpected_summaries:?}"
            ),
        );
    }

    // Once every backend has finished, read the interpreter's aggregated
    // dynamic (executed-line) coverage. `None` unless coverage was requested; an
    // empty report when requested but no interp tests ran (e.g. all-live run).
    let dynamic_coverage = if opts.coverage {
        let reports = match interp_modes_for_report
            .iter()
            .map(|mode| mode.coverage_report())
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(reports) => reports,
            Err(error) => {
                return rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    &format!("dynamic test coverage is unavailable: {error}"),
                );
            }
        };
        Some(merge_dynamic_coverage_reports(reports))
    } else {
        None
    };

    if let Err(error) = ensure_result_store(workspace, &project_root).await {
        return rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            &format!("test results store initialization failed: {error}"),
        );
    }
    let store = match workspace.test_results.read() {
        Ok(guard) => match guard.clone() {
            Some(store) => store,
            None => {
                return rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    "test results store initialization completed without a store",
                );
            }
        },
        Err(_) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                "test results store lock is poisoned",
            );
        }
    };
    let timestamp = match al_test::persistence::now_secs() {
        Ok(timestamp) => timestamp,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("could not timestamp test results: {error}"),
            );
        }
    };
    for summary in &summaries {
        for method in &summary.methods {
            let record = al_test::persistence::TestRunRecord {
                timestamp,
                codeunit_id: summary.id,
                codeunit_name: summary.name.clone(),
                method_name: method.name.clone(),
                status: method.status.clone(),
                duration_ms: method.duration_ms,
                error: method.error.clone(),
            };
            if let Err(error) = store.append(record).await {
                return rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    &format!("failed to persist test result: {error}"),
                );
            }
        }
    }

    if let Some(path) = &opts.junit_out {
        if let Err(e) = write_junit_to_path(&summaries, path).await {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("failed to write JUnit output '{}': {e}", path.display()),
            );
        }
    }
    if let Some(path) = &opts.cobertura_out {
        // Emit dynamic executed-line coverage when requested; otherwise emit
        // the historical STATIC call-graph report. The two are unambiguously
        // distinguished in the emitted XML (coverage-mode attribute + comment).
        if let Some(report) = &dynamic_coverage {
            if let Err(e) = write_cobertura_dynamic_to_path(report, path).await {
                return rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    &format!(
                        "failed to write dynamic Cobertura output '{}': {e}",
                        path.display()
                    ),
                );
            }
        } else {
            let coverage = match al_analysis::queries::test_coverage::test_coverage(workspace) {
                Ok(coverage) => coverage,
                Err(error) => {
                    return rpc_error(
                        id,
                        error_codes::INTERNAL_ERROR,
                        &format!("static coverage analysis failed: {error}"),
                    );
                }
            };
            if let Err(e) = write_cobertura_to_path(&coverage, path).await {
                return rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    &format!("failed to write Cobertura output '{}': {e}", path.display()),
                );
            }
        }
    }

    let total: usize = summaries.iter().map(|s| s.total).sum();
    let passed: usize = summaries.iter().map(|s| s.passed).sum();
    let failed: usize = summaries.iter().map(|s| s.failed).sum();
    let skipped: usize = summaries.iter().map(|s| s.skipped).sum();
    let summaries_json = match serde_json::to_value(&summaries) {
        Ok(value) => value,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("serialize test summaries failed: {error}"),
            );
        }
    };

    // Suppress unused warning on imports until junit/cobertura helpers below.
    let _ = (
        junit::write_junit::<&mut Vec<u8>>,
        cobertura::write_cobertura::<&mut Vec<u8>>,
    );

    let mut result_obj = serde_json::json!({
        "summaries": summaries_json,
        "routing": routing,
        "totals": {
            "total": total,
            "passed": passed,
            "failed": failed,
            "skipped": skipped,
        },
    });
    // Surface the interpreter's per-file executed-line and branch coverage
    // when it was requested. Absent entirely when coverage was off (existing
    // clients see exactly the prior shape).
    if let Some(report) = &dynamic_coverage {
        if let Some(map) = result_obj.as_object_mut() {
            map.insert("coverage".to_string(), dynamic_coverage_to_json(report));
        }
    }

    Response {
        id,
        result: Some(result_obj),
        error: None,
        ..Default::default()
    }
}

/// Merge coverage gathered by the pure and record-enabled interpreter
/// sessions. A batch may contain both capability tiers, but callers should see
/// one stable dynamic-coverage report.
fn merge_dynamic_coverage_reports(
    reports: impl IntoIterator<Item = al_runtime::interpreter::coverage::DynamicCoverageReport>,
) -> al_runtime::interpreter::coverage::DynamicCoverageReport {
    use al_runtime::interpreter::coverage::{
        BranchCoverage, ConditionObservationCoverage, FileCoverage, McdcCoverage, PathCoverage,
    };
    use std::collections::{BTreeMap, BTreeSet};

    #[derive(Default)]
    struct BranchTally {
        then_taken: u64,
        else_taken: u64,
        paths: BTreeMap<String, u64>,
        condition_observations: BTreeMap<(Vec<bool>, bool), u64>,
    }
    type BranchTallies = BTreeMap<u32, BranchTally>;
    type FileTallies = (BTreeSet<u32>, BranchTallies);
    let mut files: BTreeMap<String, FileTallies> = BTreeMap::new();
    for report in reports {
        for file in report.files {
            let entry = files.entry(file.file).or_default();
            entry.0.extend(file.executed_lines);
            for branch in file.branches {
                let tally = entry.1.entry(branch.line).or_default();
                tally.then_taken += branch.then_taken;
                tally.else_taken += branch.else_taken;
                for path in branch.paths {
                    *tally.paths.entry(path.path).or_insert(0) += path.hits;
                }
                if let Some(mcdc) = branch.mcdc {
                    for observation in mcdc.observations {
                        *tally
                            .condition_observations
                            .entry((observation.conditions, observation.outcome))
                            .or_insert(0) += observation.hits;
                    }
                }
            }
        }
    }

    al_runtime::interpreter::coverage::DynamicCoverageReport {
        files: files
            .into_iter()
            .map(|(file, (executed_lines, branches))| FileCoverage {
                file,
                executed_lines: executed_lines.into_iter().collect(),
                branches: branches
                    .into_iter()
                    .map(|(line, tally)| BranchCoverage {
                        line,
                        then_taken: tally.then_taken,
                        else_taken: tally.else_taken,
                        paths: tally
                            .paths
                            .into_iter()
                            .map(|(path, hits)| PathCoverage { path, hits })
                            .collect(),
                        mcdc: McdcCoverage::from_observations(
                            tally.condition_observations.into_iter().map(
                                |((conditions, outcome), hits)| ConditionObservationCoverage {
                                    conditions,
                                    outcome,
                                    hits,
                                },
                            ),
                        ),
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// Serialize a [`DynamicCoverageReport`] into the `tests.run_batch`
/// result shape: `{ mode, files: [{ file, executedLines, branches: [{ line,
/// thenTaken, elseTaken, paths }] }] }`. Built here rather than via `Serialize` on the
/// al-runtime type so the runtime crate stays free of a wire-format commitment.
fn dynamic_coverage_to_json(
    report: &al_runtime::interpreter::coverage::DynamicCoverageReport,
) -> serde_json::Value {
    let files: Vec<serde_json::Value> = report
        .files
        .iter()
        .map(|f| {
            let branches: Vec<serde_json::Value> = f
                .branches
                .iter()
                .map(|b| {
                    serde_json::json!({
                        "line": b.line,
                        "thenTaken": b.then_taken,
                        "elseTaken": b.else_taken,
                        "paths": b.paths.iter().map(|path| serde_json::json!({
                            "path": path.path,
                            "hits": path.hits,
                        })).collect::<Vec<_>>(),
                        "mcdc": b.mcdc.as_ref().map(|mcdc| serde_json::json!({
                            "covered": mcdc.covered_count(),
                            "total": mcdc.conditions.len(),
                            "conditions": mcdc.conditions.iter().map(|condition| serde_json::json!({
                                "index": condition.index,
                                "covered": condition.covered,
                            })).collect::<Vec<_>>(),
                            "observations": mcdc.observations.iter().map(|observation| serde_json::json!({
                                "conditions": observation.conditions,
                                "outcome": observation.outcome,
                                "hits": observation.hits,
                            })).collect::<Vec<_>>(),
                        })),
                    })
                })
                .collect();
            serde_json::json!({
                "file": f.file,
                "executedLines": f.executed_lines,
                "branches": branches,
            })
        })
        .collect();
    serde_json::json!({
        "mode": "dynamic-executed-lines",
        "files": files,
    })
}

async fn write_cobertura_dynamic_to_path(
    report: &al_runtime::interpreter::coverage::DynamicCoverageReport,
    path: &std::path::Path,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut buf = Vec::new();
    al_test::output::cobertura::write_cobertura_dynamic(report, &mut buf)?;
    crate::server::daemon::containment::write_no_follow(path, buf).await
}
pub(in crate::server::daemon) async fn dispatch_tests_run_auto(
    workspace: &std::sync::Arc<Workspace>,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let discovered = match al_analysis::queries::tests::discover_tests(workspace) {
        Ok(discovered) => discovered,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("test discovery failed: {error}"),
            );
        }
    };
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
    let codeunit_filter = match params.get("codeunitId") {
        None => None,
        Some(value) => match value.as_i64().and_then(|value| i32::try_from(value).ok()) {
            Some(codeunit_id) if codeunit_id > 0 => Some(codeunit_id),
            _ => {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    "'codeunitId' is out of range (must be a positive i32 AL object ID)",
                );
            }
        },
    };
    let method_filter = match optional_non_empty_string(params, "methodName") {
        Ok(method) => method,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    if method_filter.is_some() && codeunit_filter.is_none() {
        return rpc_error(
            id,
            error_codes::INVALID_PARAMS,
            "'methodName' requires 'codeunitId'",
        );
    }

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

    if let (Some(codeunit_id), Some(method)) = (codeunit_filter, method_filter) {
        return match store.last_for(codeunit_id, method).await {
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
    let filtered: Vec<_> = match codeunit_filter {
        Some(codeunit_id) => all
            .into_iter()
            .filter(|result| result.codeunit_id == codeunit_id)
            .collect(),
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
) -> Result<(), al_test::PersistenceError> {
    {
        let guard = workspace.test_results.read().map_err(|_| {
            al_test::PersistenceError::Io(std::io::Error::other("test_results lock poisoned"))
        })?;
        if guard.is_some() {
            return Ok(());
        }
    }
    let store = al_test::TestResultStore::open_for_project(project_root).await?;
    let mut guard = workspace.test_results.write().map_err(|_| {
        al_test::PersistenceError::Io(std::io::Error::other("test_results lock poisoned"))
    })?;
    *guard = Some(std::sync::Arc::new(store));
    Ok(())
}
async fn write_junit_to_path(
    summaries: &[al_test::result::TestCodeunitResult],
    path: &std::path::Path,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut buf = Vec::new();
    al_test::output::junit::write_junit(summaries, &mut buf)?;
    crate::server::daemon::containment::write_no_follow(path, buf).await
}
async fn write_cobertura_to_path(
    report: &al_analysis::queries::test_coverage::CoverageReport,
    path: &std::path::Path,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut buf = Vec::new();
    al_test::output::cobertura::write_cobertura(report, &mut buf)?;
    crate::server::daemon::containment::write_no_follow(path, buf).await
}
pub(in crate::server::daemon) fn dispatch_tests_affected(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let changed_files = match optional_array(params, "changedFiles") {
        Ok(Some(files)) => files,
        Ok(None) => {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                "Missing 'changedFiles' (array of paths)",
            );
        }
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let mut paths = Vec::with_capacity(changed_files.len());
    let mut seen = std::collections::HashSet::new();
    for (index, value) in changed_files.iter().enumerate() {
        let Some(path) = value.as_str().filter(|path| !path.trim().is_empty()) else {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                &format!("changedFiles[{index}] must be a non-empty string path"),
            );
        };
        if !seen.insert(path.to_string()) {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                &format!("changedFiles[{index}] duplicates an earlier path"),
            );
        }
        paths.push(path.to_string());
    }
    let affected = match al_analysis::queries::tests::affected_tests(workspace, &paths) {
        Ok(affected) => affected,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("affected-test analysis failed: {error}"),
            );
        }
    };
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
    use al_test::router;
    let results = match router::classify_all(workspace) {
        Ok(results) => results,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("test routing analysis failed: {error}"),
            );
        }
    };
    let json: Vec<serde_json::Value> = results
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "codeunitId": r.codeunit_id,
                "codeunitName": r.codeunit_name,
                "methodName": r.method_name,
                "decision": r.decision.as_str(),
                "runsLocally": r.decision.runs_locally(),
                "execution": r.decision.execution_note(),
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

/// - `timeoutMs` (optional, u64): per-variant timeout in ms.
///
/// Returns: serialized `MutationReport` JSON.
pub(in crate::server::daemon) async fn dispatch_tests_mutate(
    workspace: &std::sync::Arc<Workspace>,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    use al_protocol::jsonrpc::error_codes;
    use al_test::mutate::MutationOptions;

    let parallel = match optional_bool_param(params, "parallel", false) {
        Ok(parallel) => parallel,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let timeout_ms = match optional_timeout_ms(params) {
        Ok(timeout) => timeout,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let requested_files = match optional_array(params, "files") {
        Ok(None) => None,
        Ok(Some(values)) => {
            if values.is_empty() {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    "'files' must not be empty when supplied",
                );
            }
            let mut files = Vec::with_capacity(values.len());
            let mut seen = std::collections::HashSet::new();
            for (index, value) in values.iter().enumerate() {
                let Some(file) = value
                    .as_str()
                    .map(str::trim)
                    .filter(|file| !file.is_empty())
                else {
                    return rpc_error(
                        id,
                        error_codes::INVALID_PARAMS,
                        &format!("files[{index}] must be a non-empty string path"),
                    );
                };
                if !seen.insert(file.to_string()) {
                    return rpc_error(
                        id,
                        error_codes::INVALID_PARAMS,
                        &format!("files[{index}] duplicates an earlier path"),
                    );
                }
                files.push(file.to_string());
            }
            Some(files)
        }
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let project_root = {
        let project = workspace.project.read().await;
        let Some(project) = project.as_ref() else {
            return rpc_error(id, error_codes::INTERNAL_ERROR, ERR_NO_PROJECT);
        };
        project.root.clone()
    };
    let files = match requested_files {
        Some(files) => match resolve_mutation_file_allowlist(workspace, &project_root, files) {
            Ok(files) => Some(files),
            Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
        },
        None => None,
    };

    let opts = MutationOptions {
        affected_only: true,
        parallel,
        timeout_ms,
        files,
    };

    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = al_test::mutate::run_mutation_testing(workspace, opts, tx).await;
    if let Err(error) = drain.await {
        return rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            &format!("mutation event worker failed: {error}"),
        );
    }
    let report = match report {
        Ok(report) => report,
        Err(
            e @ (al_test::mutate::MutationError::NoTestFiles
            | al_test::mutate::MutationError::NoSelectedReachableFiles { .. }
            | al_test::mutate::MutationError::NoMutationFiles
            | al_test::mutate::MutationError::NoMutationVariants
            | al_test::mutate::MutationError::ParseError { .. }),
        ) => {
            return rpc_error(id, error_codes::CODE_ANALYSIS_ERROR, &e.to_string());
        }
        Err(e) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("mutation run failed: {e}"),
            );
        }
    };

    match serde_json::to_value(&report) {
        Ok(mut value) => {
            if let Some(object) = value.as_object_mut() {
                object.insert(
                    "mutationScore".to_string(),
                    report
                        .mutation_score()
                        .and_then(serde_json::Number::from_f64)
                        .map(serde_json::Value::Number)
                        .unwrap_or(serde_json::Value::Null),
                );
                object.insert(
                    "unscored".to_string(),
                    serde_json::json!(report.unscored_count()),
                );
            }
            Response {
                id,
                result: Some(value),
                error: None,
                ..Default::default()
            }
        }
        Err(e) => rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            &format!("Serialization error: {e}"),
        ),
    }
}

mod snapshot;
pub(in crate::server::daemon) use snapshot::*;

#[cfg(test)]
mod tests;
