//! `tests.snapshot*`: capture, validate, replay and diff test snapshots.

use super::*;

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

    // The launch configuration this token is about to be sent to ships in the
    // repository, so the authorisation inside `acquire_bc_token` is what stops
    // a cloned project from collecting the user's cached credential.
    let access_token = match crate::server::daemon::debug_dispatch::acquire_bc_token(
        workspace,
        &debug,
        supplied_token,
        al_project::trust::TargetSource::Repository,
    )
    .await
    {
        Ok(token) => token,
        Err((code, message)) => return rpc_error(id, code, &message),
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
    if let Err(error) = crate::server::daemon::containment::write_no_follow(&output, bytes).await {
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
