//! Test-runner and coverage dispatchers.

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
    let absolute = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        project_root.join(requested)
    };

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
                let file = existing.file_name()?;
                // Build the tail by PREPENDING each not-yet-existing component.
                // `PathBuf::from(file).push(&tail)` when `tail` is empty appends
                // a trailing separator (`j.xml` -> `j.xml/`), so the resolved
                // path ended in a separator and was later treated as a
                // directory — `write_junit_to_path` then `create_dir_all`'d the
                // file-as-directory and the report write failed silently while
                // the command still reported success. Only
                // push when there is an existing tail to append.
                tail = if tail.as_os_str().is_empty() {
                    PathBuf::from(file)
                } else {
                    let mut new_tail = PathBuf::from(file);
                    new_tail.push(&tail);
                    new_tail
                };
                existing = existing.parent()?;
            }
        }
    };
    let resolved = canonical_existing.join(&tail);

    if resolved.starts_with(&project_canonical) {
        Some(resolved)
    } else {
        None
    }
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
        return rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            &format!(
                "test backends reported errors: {}",
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
    tokio::fs::write(path, buf).await
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
    tokio::fs::write(path, buf).await
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
    tokio::fs::write(path, buf).await
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

pub(in crate::server::daemon) async fn dispatch_tests_snapshot_validate(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let path = match optional_non_empty_string(params, "snapshotPath") {
        Ok(Some(path)) => path,
        Ok(None) => return rpc_error(id, error_codes::INVALID_PARAMS, "Missing 'snapshotPath'"),
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let project_root = match snapshot_project_root(workspace).await {
        Ok(root) => root,
        Err(message) => return rpc_error(id, error_codes::INTERNAL_ERROR, &message),
    };
    let path = match resolve_existing_snapshot_path(path, &project_root) {
        Ok(path) => path,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("read snapshot failed: {error}"),
            );
        }
    };
    let mut snapshot = match al_snapshot::deserialize_snapshot(&bytes) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                &format!("snapshot parse failed: {error}"),
            );
        }
    };
    if let Err(message) = normalize_snapshot_sample_paths(&mut snapshot, &project_root) {
        return rpc_error(id, error_codes::INVALID_PARAMS, &message);
    }

    Response {
        id,
        result: Some(serde_json::json!({
            "valid": true,
            "sampleCount": snapshot.samples.len(),
            "codeunitId": snapshot.codeunit_id,
            "methodName": snapshot.method_name,
            "bcVersion": snapshot.bc_version,
        })),
        error: None,
        ..Default::default()
    }
}

async fn snapshot_project_root(workspace: &Workspace) -> Result<PathBuf, String> {
    let root = workspace
        .project
        .read()
        .await
        .as_ref()
        .map(|project| project.root.clone())
        .ok_or_else(|| ERR_NO_PROJECT.to_string())?;
    root.canonicalize()
        .map_err(|error| format!("resolve project root failed: {error}"))
}

fn resolve_existing_snapshot_path(
    requested: &str,
    project_root: &std::path::Path,
) -> Result<PathBuf, String> {
    let path = std::path::Path::new(requested);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    };
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("resolve snapshot path failed: {error}"))?;
    if !canonical.starts_with(project_root) {
        return Err("snapshot path escapes the project root".to_string());
    }
    if !canonical.is_file() {
        return Err("snapshot path is not a regular file".to_string());
    }
    Ok(canonical)
}

fn normalize_snapshot_sample_paths(
    snapshot: &mut al_snapshot::Snapshot,
    project_root: &std::path::Path,
) -> Result<(), String> {
    for sample in &mut snapshot.samples {
        let path = std::path::Path::new(&sample.file);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            project_root.join(path)
        };
        let canonical = path.canonicalize().map_err(|error| {
            format!(
                "snapshot breakpoint source '{}' cannot be resolved: {error}",
                sample.file
            )
        })?;
        let relative = canonical.strip_prefix(project_root).map_err(|_| {
            format!(
                "snapshot breakpoint source '{}' escapes the project root",
                sample.file
            )
        })?;
        if !canonical.is_file() {
            return Err(format!(
                "snapshot breakpoint source '{}' is not a regular file",
                sample.file
            ));
        }
        sample.file = relative.to_string_lossy().replace('\\', "/");
    }
    al_snapshot::validate_snapshot(snapshot).map_err(|error| error.to_string())
}

pub(in crate::server::daemon) async fn dispatch_tests_snapshot_capture(
    workspace: &std::sync::Arc<Workspace>,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    use al_bc::launch::find_launch_config;
    use al_test::backends::snapshot::{
        capture_live_snapshot, LiveSnapshotRequest, SnapshotBreakpoint,
    };
    use sha2::{Digest, Sha256};

    let codeunit_id = match params
        .get("codeunitId")
        .and_then(|value| value.as_i64())
        .and_then(|value| i32::try_from(value).ok())
    {
        Some(value) if value > 0 => value,
        _ => {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                "Missing or invalid 'codeunitId'",
            );
        }
    };
    let codeunit_name = match optional_non_empty_string(params, "codeunitName") {
        Ok(Some(value)) => value.to_string(),
        Ok(None) => return rpc_error(id, error_codes::INVALID_PARAMS, "Missing 'codeunitName'"),
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let method_name = match optional_non_empty_string(params, "methodName") {
        Ok(Some(value)) => value.to_string(),
        Ok(None) => return rpc_error(id, error_codes::INVALID_PARAMS, "Missing 'methodName'"),
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let bc_version = match optional_non_empty_string(params, "bcVersion") {
        Ok(Some(value)) => value.to_string(),
        Ok(None) => {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                "Missing 'bcVersion'; capture metadata must identify the live BC runtime",
            );
        }
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let raw_breakpoints = match optional_array(params, "breakpoints") {
        Ok(Some(values)) if !values.is_empty() && values.len() <= 10_000 => values,
        Ok(Some(values)) if values.len() > 10_000 => {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                "'breakpoints' must contain no more than 10000 entries",
            );
        }
        Ok(_) => {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                "Missing 'breakpoints' (non-empty array of {file,line})",
            );
        }
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let output_path = match optional_non_empty_string(params, "outputPath") {
        Ok(Some(path)) => path,
        Ok(None) => return rpc_error(id, error_codes::INVALID_PARAMS, "Missing 'outputPath'"),
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    if !output_path.to_ascii_lowercase().ends_with(".snap.json") {
        return rpc_error(
            id,
            error_codes::INVALID_PARAMS,
            "'outputPath' must end with .snap.json",
        );
    }
    let config_name = match optional_non_empty_string(params, "config") {
        Ok(config) => config,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let supplied_token = match params.get("accessToken") {
        None => "",
        Some(value) => match value.as_str() {
            Some(token) => token,
            None => {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    "'accessToken' must be a string when supplied",
                );
            }
        },
    };
    let timeout_ms = match optional_timeout_ms(params) {
        Ok(timeout) => timeout.unwrap_or(300_000),
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };

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
    let project_root = match project_root.canonicalize() {
        Ok(root) => root,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("resolve project root failed: {error}"),
            );
        }
    };
    let output = match resolve_output_path_within_project(
        std::path::Path::new(output_path),
        &project_root,
    ) {
        Some(path) => path,
        None => {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                "'outputPath' path escapes the project root",
            );
        }
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
    let matching_codeunits = discovered
        .iter()
        .filter(|codeunit| codeunit.id == codeunit_id)
        .collect::<Vec<_>>();
    let codeunit = match matching_codeunits.as_slice() {
        [codeunit] => codeunit,
        [] => {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                "snapshot codeunit is not present in the workspace test index",
            );
        }
        _ => {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                "snapshot codeunit ID is ambiguous in the workspace test index",
            );
        }
    };
    if !codeunit.name.eq_ignore_ascii_case(&codeunit_name) {
        return rpc_error(
            id,
            error_codes::INVALID_PARAMS,
            &format!(
                "codeunitName '{codeunit_name}' does not match indexed codeunit '{}'",
                codeunit.name
            ),
        );
    }
    if !codeunit
        .tests
        .iter()
        .any(|test| test.name.eq_ignore_ascii_case(&method_name))
    {
        return rpc_error(
            id,
            error_codes::INVALID_PARAMS,
            &format!(
                "test method '{method_name}' is not present in codeunit '{}'",
                codeunit.name
            ),
        );
    }

    let mut breakpoints = Vec::with_capacity(raw_breakpoints.len());
    let mut source_files = Vec::new();
    let mut seen_breakpoints = std::collections::HashSet::new();
    for raw in raw_breakpoints {
        let file = match raw
            .get("file")
            .and_then(|value| value.as_str())
            .filter(|file| !file.trim().is_empty())
        {
            Some(file) => file,
            None => {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    "Each breakpoint requires 'file'",
                );
            }
        };
        let line = match raw
            .get("line")
            .and_then(|value| value.as_u64())
            .and_then(|value| u32::try_from(value).ok())
            .filter(|line| *line > 0)
        {
            Some(line) => line,
            None => {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    "Each breakpoint requires a positive 1-based 'line'",
                );
            }
        };
        let file_path = if std::path::Path::new(file).is_absolute() {
            PathBuf::from(file)
        } else {
            project_root.join(file)
        };
        let file_path = match file_path.canonicalize() {
            Ok(path) if path.starts_with(&project_root) => path,
            _ => {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    "Breakpoint file must exist inside the project root",
                );
            }
        };
        let Some(info) = workspace.file_index.object_info.get(&file_path) else {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                "Breakpoint file is not an indexed AL object",
            );
        };
        let Some(source) = workspace.file_index.files.get(&file_path) else {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                "Breakpoint object metadata has no matching indexed source",
            );
        };
        if usize::try_from(line)
            .ok()
            .is_none_or(|line| line > source.lines().count())
        {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                "Breakpoint line is outside the indexed source file",
            );
        }
        let Some(object_id) = info.value().id.and_then(|value| i32::try_from(value).ok()) else {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                "Breakpoint file has no valid AL object ID",
            );
        };
        let object_type = al_dap::dap::native_dap::kind_to_object_type(&info.value().kind);
        if !seen_breakpoints.insert((file_path.clone(), line)) {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                "Duplicate breakpoint file/line",
            );
        }
        source_files.push(file_path.clone());
        let condition = match raw.get("condition") {
            None => None,
            Some(value) => match value
                .as_str()
                .filter(|condition| !condition.trim().is_empty())
            {
                Some(condition) => Some(condition.to_string()),
                None => {
                    return rpc_error(
                        id,
                        error_codes::INVALID_PARAMS,
                        "Breakpoint 'condition' must be a non-empty string when supplied",
                    );
                }
            },
        };
        breakpoints.push(SnapshotBreakpoint {
            file: file_path.to_string_lossy().into_owned(),
            line,
            object_type,
            object_id,
            condition,
        });
    }
    source_files.sort();
    source_files.dedup();
    let mut source_hasher = Sha256::new();
    for file in &source_files {
        let stable_path = match file.strip_prefix(&project_root) {
            Ok(path) => path,
            Err(_) => {
                return rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    "validated breakpoint path no longer belongs to the project",
                );
            }
        };
        source_hasher.update(stable_path.to_string_lossy().replace('\\', "/").as_bytes());
        source_hasher.update([0]);
        let bytes = match tokio::fs::read(file).await {
            Ok(bytes) => bytes,
            Err(error) => {
                return rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    &format!("read breakpoint source failed: {error}"),
                );
            }
        };
        source_hasher.update(bytes);
        source_hasher.update([0]);
    }
    let source_hash = format!("{:x}", source_hasher.finalize());

    let launch = match find_launch_config(&project_root) {
        Ok(Some(launch)) => launch,
        Ok(None) => {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                "No debug configuration found in project (.zed/debug.json or .vscode/launch.json)",
            );
        }
        Err(error) => return rpc_error(id, error_codes::INVALID_PARAMS, &error.to_string()),
    };
    let server = match crate::server::daemon::debug_dispatch::pick_named_config(
        &launch.configs,
        config_name,
    ) {
        Ok(config) => config.clone(),
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let mut debug =
        match crate::server::daemon::debug_dispatch::resolve_debug_config(workspace, params) {
            Ok(config) => config,
            Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
        };
    debug.break_on_next = Some("WebServiceClient".to_string());
    debug.launch_browser = false;
    if let Err(message) = debug.validate_native() {
        return rpc_error(id, error_codes::INVALID_PARAMS, &message);
    }

    let access_token = if supplied_token.is_empty()
        && crate::server::daemon::debug_dispatch::debug_uses_oauth(&debug)
    {
        match al_bc::http_auth::access_token_from_env() {
            Ok(Some(token)) => token,
            Ok(None) => {
                let client = reqwest::Client::new();
                match al_symbols::oauth::acquire_token(&client, &debug.tenant, |message| {
                    tracing::info!("snapshot authentication: {message}");
                })
                .await
                {
                    Ok(token) => token,
                    Err(error) => {
                        return rpc_error(
                            id,
                            error_codes::INTERNAL_ERROR,
                            &format!("snapshot authentication failed: {error}"),
                        );
                    }
                }
            }
            Err(error) => {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    &format!("Invalid bearer-token environment: {error}"),
                );
            }
        }
    } else {
        supplied_token.to_string()
    };

    let timeout = std::time::Duration::from_millis(timeout_ms);
    let (mut snapshot, test_result) = match capture_live_snapshot(LiveSnapshotRequest {
        server,
        debug,
        access_token,
        codeunit_id,
        codeunit_name,
        method_name,
        bc_version,
        source_hash,
        breakpoints,
        timeout,
    })
    .await
    {
        Ok(result) => result,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("snapshot capture failed: {error}"),
            );
        }
    };
    for sample in &mut snapshot.samples {
        let path = std::path::Path::new(&sample.file);
        if let Ok(relative) = path.strip_prefix(&project_root) {
            sample.file = relative.to_string_lossy().replace('\\', "/");
        }
    }
    let bytes = match al_snapshot::serialize_snapshot(&snapshot) {
        Ok(bytes) => bytes,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("snapshot serialization failed: {error}"),
            );
        }
    };
    if let Some(parent) = output.parent() {
        if let Err(error) = tokio::fs::create_dir_all(parent).await {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("create snapshot output directory failed: {error}"),
            );
        }
    }
    if let Err(error) = tokio::fs::write(&output, bytes).await {
        return rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            &format!("write snapshot failed: {error}"),
        );
    }

    Response {
        id,
        result: Some(serde_json::json!({
            "captured": true,
            "snapshotPath": output,
            "sampleCount": snapshot.samples.len(),
            "runId": snapshot.run_id,
            "codeunitId": snapshot.codeunit_id,
            "methodName": snapshot.method_name,
            "bcVersion": snapshot.bc_version,
            "sourceHash": snapshot.source_hash,
            "testResult": test_result,
        })),
        error: None,
        ..Default::default()
    }
}

pub(in crate::server::daemon) async fn dispatch_tests_snapshot_replay(
    workspace: &std::sync::Arc<Workspace>,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let snapshot_path = match optional_non_empty_string(params, "snapshotPath") {
        Ok(Some(path)) => path,
        Ok(None) => return rpc_error(id, error_codes::INVALID_PARAMS, "Missing 'snapshotPath'"),
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let bc_version = match optional_non_empty_string(params, "bcVersion") {
        Ok(Some(version)) => version,
        Ok(None) => {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                "Missing 'bcVersion'; replay metadata must identify the live BC runtime",
            );
        }
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let project_root = match snapshot_project_root(workspace).await {
        Ok(root) => root,
        Err(message) => return rpc_error(id, error_codes::INTERNAL_ERROR, &message),
    };
    let snapshot_path = match resolve_existing_snapshot_path(snapshot_path, &project_root) {
        Ok(path) => path,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let bytes = match tokio::fs::read(&snapshot_path).await {
        Ok(bytes) => bytes,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("read baseline snapshot failed: {error}"),
            );
        }
    };
    let mut baseline = match al_snapshot::deserialize_snapshot(&bytes) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                &format!("baseline snapshot parse failed: {error}"),
            );
        }
    };
    if baseline.samples.is_empty() {
        return rpc_error(
            id,
            error_codes::INVALID_PARAMS,
            "baseline snapshot has no samples to replay",
        );
    }
    if let Err(message) = normalize_snapshot_sample_paths(&mut baseline, &project_root) {
        return rpc_error(id, error_codes::INVALID_PARAMS, &message);
    }

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
    let matching_tests = discovered
        .into_iter()
        .filter(|codeunit| {
            codeunit.id == baseline.codeunit_id
                && codeunit
                    .tests
                    .iter()
                    .any(|test| test.name.eq_ignore_ascii_case(&baseline.method_name))
        })
        .collect::<Vec<_>>();
    let codeunit = match matching_tests.as_slice() {
        [codeunit] => codeunit,
        [] => {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                "baseline test codeunit/method is not present in the current workspace index",
            );
        }
        _ => {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                "baseline test codeunit ID is ambiguous in the current workspace",
            );
        }
    };

    let mut unique_breakpoints = std::collections::BTreeMap::<(String, u32), Option<String>>::new();
    for sample in &baseline.samples {
        unique_breakpoints
            .entry((sample.file.clone(), sample.line))
            .or_insert_with(|| sample.condition.clone());
    }
    let breakpoints = unique_breakpoints
        .into_iter()
        .map(|((file, line), condition)| {
            let mut value = serde_json::json!({ "file": file, "line": line });
            if let Some(condition) = condition {
                value["condition"] = serde_json::Value::String(condition);
            }
            value
        })
        .collect::<Vec<_>>();

    let replay_nonce = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos(),
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("system clock cannot create replay nonce: {error}"),
            );
        }
    };
    let replay_output = project_root
        .join("target/al-snapshot-replay")
        .join(format!("{}-{replay_nonce}.snap.json", std::process::id()));
    let mut capture_params = serde_json::json!({
        "codeunitId": baseline.codeunit_id,
        "codeunitName": codeunit.name,
        "methodName": baseline.method_name,
        "bcVersion": bc_version,
        "breakpoints": breakpoints,
        "outputPath": replay_output,
    });
    for name in ["config", "timeoutMs", "accessToken"] {
        if let Some(value) = params.get(name) {
            capture_params[name] = value.clone();
        }
    }

    let capture = dispatch_tests_snapshot_capture(workspace, id, &capture_params).await;
    if capture.error.is_some() {
        if let Err(error) = tokio::fs::remove_file(&replay_output).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    path = %replay_output.display(),
                    %error,
                    "failed to remove unsuccessful replay output"
                );
            }
        }
        return capture;
    }
    let test_result = match capture
        .result
        .as_ref()
        .and_then(|result| result.get("testResult"))
        .cloned()
    {
        Some(result) => result,
        None => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                "snapshot capture succeeded without a testResult",
            );
        }
    };
    let observed_bytes = match tokio::fs::read(&replay_output).await {
        Ok(bytes) => bytes,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("read replay snapshot failed: {error}"),
            );
        }
    };
    if let Err(error) = tokio::fs::remove_file(&replay_output).await {
        return rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            &format!("remove temporary replay snapshot failed: {error}"),
        );
    }
    if let Some(parent) = replay_output.parent() {
        if let Err(error) = tokio::fs::remove_dir(parent).await {
            if !matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
            ) {
                tracing::warn!(
                    path = %parent.display(),
                    %error,
                    "failed to remove replay temporary directory"
                );
            }
        }
    }
    let mut observed = match al_snapshot::deserialize_snapshot(&observed_bytes) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("replay snapshot parse failed: {error}"),
            );
        }
    };
    if let Err(message) = normalize_snapshot_sample_paths(&mut observed, &project_root) {
        return rpc_error(id, error_codes::INTERNAL_ERROR, &message);
    }
    let divergences = al_snapshot::diff_snapshots(&baseline, &observed);

    Response {
        id,
        result: Some(serde_json::json!({
            "replayed": true,
            "matched": divergences.is_empty(),
            "divergences": divergences,
            "baseline": {
                "snapshotPath": snapshot_path,
                "sampleCount": baseline.samples.len(),
                "bcVersion": baseline.bc_version,
                "sourceHash": baseline.source_hash,
            },
            "observed": {
                "sampleCount": observed.samples.len(),
                "bcVersion": observed.bc_version,
                "sourceHash": observed.source_hash,
                "testResult": test_result,
            },
        })),
        error: None,
        ..Default::default()
    }
}

pub(in crate::server::daemon) async fn dispatch_tests_snapshot_diff(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let path_a = match optional_non_empty_string(params, "pathA") {
        Ok(Some(path)) => path,
        Ok(None) => return rpc_error(id, error_codes::INVALID_PARAMS, "Missing 'pathA'"),
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let path_b = match optional_non_empty_string(params, "pathB") {
        Ok(Some(path)) => path,
        Ok(None) => return rpc_error(id, error_codes::INVALID_PARAMS, "Missing 'pathB'"),
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let project_root = match snapshot_project_root(workspace).await {
        Ok(root) => root,
        Err(message) => return rpc_error(id, error_codes::INTERNAL_ERROR, &message),
    };
    let read = async |p: &str| -> Result<al_snapshot::format::Snapshot, String> {
        let path = resolve_existing_snapshot_path(p, &project_root)?;
        let bytes = tokio::fs::read(path).await.map_err(|e| e.to_string())?;
        let mut snapshot =
            al_snapshot::format::deserialize_snapshot(&bytes).map_err(|e| e.to_string())?;
        normalize_snapshot_sample_paths(&mut snapshot, &project_root)?;
        Ok(snapshot)
    };
    let a = match read(path_a).await {
        Ok(s) => s,
        Err(e) => return rpc_error(id, error_codes::INVALID_PARAMS, &format!("pathA: {e}")),
    };
    let b = match read(path_b).await {
        Ok(s) => s,
        Err(e) => return rpc_error(id, error_codes::INVALID_PARAMS, &format!("pathB: {e}")),
    };
    let divergences = al_snapshot::diff::diff_snapshots(&a, &b);
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

#[cfg(test)]
mod tests {
    use super::*;
    use al_protocol::jsonrpc::error_codes;
    use al_workspace::Workspace;

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
    async fn ws_with_branch_codeunit(
        tmp: &tempfile::TempDir,
    ) -> (std::sync::Arc<Workspace>, u32, u32) {
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
        let only_a =
            dispatch_tests_snapshot_diff(&ws, 1, &serde_json::json!({ "pathA": "/x" })).await;
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
}
