//! Snapshot debugging of a Business Central environment: the daemon method
//! `snapshot` with the commands `start`, `status`, `finish`, `list` and `show`.
//!
//! Routes checked before writing this: the AL host's LSP route and Microsoft's
//! snapshot MCP proxy both need the AL extension or a server side feature, so
//! the commands call the snapshot debugger REST endpoints natively through
//! `al_dap::dap::bc_debug::snapshot_debugger`, which reuses the debugger's
//! `BcDebugConfig` and token path. Configuration lookup, authorisation and
//! token acquisition are the ones `debug start` uses (`debug_dispatch`), and the
//! user name and BaseApp lookups are the Automation API calls in
//! `bc_debug::automation`. Nothing here sends a request of its own.
//!
//! A session is remembered between commands in `al_bc::snapshot::state`. The
//! access token is never stored: every command gets one again.

use std::path::{Path, PathBuf};

use al_bc::snapshot::archive::{render_stops, RenderOptions, SnapshotArchive};
use al_bc::snapshot::state::{SessionRecord, SessionStore, SnappointRecord};
use al_dap::dap::bc_debug::snapshot_capture::{
    self, environment_version_warnings, parse_client_type, parse_verbosity, CaptureError,
    FinishResult, StartRequest, VersionInfo,
};
use al_dap::dap::bc_debug::snapshot_debugger::{guard_production, SnapshotDebuggerError};
use al_dap::dap::bc_debug::BcDebugConfig;
use al_protocol::jsonrpc::{error_codes, Response};
use al_workspace::Workspace;
use serde_json::{json, Value};

use super::containment::{resolve_param_within_project, resolve_write_param_within_project};
use super::debug_dispatch::{acquire_bc_token, debug_config_from_server_config, pick_named_config};
use crate::server::dap_mode::snappoints::{
    alpackages_app_version, parse_snap_spec, resolve_snappoint, ResolvedSnappoint, SnapSource,
};

type Failure = (i32, String);

fn invalid(message: impl Into<String>) -> Failure {
    (error_codes::INVALID_PARAMS, message.into())
}

fn internal(message: impl Into<String>) -> Failure {
    (error_codes::INTERNAL_ERROR, message.into())
}

fn answer(id: u64, outcome: Result<Value, Failure>) -> Response {
    match outcome {
        Ok(value) => Response::ok(id, value),
        Err((code, message)) => Response::error(id, code, message),
    }
}

fn snapshot_failure(error: SnapshotDebuggerError) -> Failure {
    match error {
        SnapshotDebuggerError::Production => invalid(error.to_string()),
        other => internal(other.to_string()),
    }
}

fn capture_failure(error: CaptureError) -> Failure {
    match error {
        CaptureError::Invalid(message) => invalid(message),
        CaptureError::Internal(message) => internal(message),
        CaptureError::Snapshot(error) => snapshot_failure(error),
    }
}

#[cfg(test)]
thread_local! {
    pub(super) static TEST_STORE: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    pub(super) static TEST_AUTOMATION_BASE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Point the session store of the current test thread at `dir`.
#[cfg(test)]
pub(crate) fn use_test_store(dir: PathBuf) {
    TEST_STORE.with(|cell| *cell.borrow_mut() = Some(dir));
}

fn session_store() -> Result<SessionStore, Failure> {
    #[cfg(test)]
    if let Some(dir) = TEST_STORE.with(|cell| cell.borrow().clone()) {
        return Ok(SessionStore::at(dir));
    }
    SessionStore::default_location()
        .ok_or_else(|| internal("no local data directory for the snapshot session store"))
}

/// An explicit Automation API base for tests. Production code always derives
/// the base from the configuration.
fn automation_base_override() -> Option<String> {
    #[cfg(test)]
    {
        TEST_AUTOMATION_BASE.with(|cell| cell.borrow().clone())
    }
    #[cfg(not(test))]
    {
        None
    }
}

fn opt_str<'a>(params: &'a Value, key: &str) -> Result<Option<&'a str>, Failure> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.trim().is_empty() => {
            Err(invalid(format!("'{key}' must not be empty when supplied")))
        }
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(invalid(format!("'{key}' must be a string when supplied"))),
    }
}

fn opt_bool(params: &Value, key: &str) -> Result<bool, Failure> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(invalid(format!("'{key}' must be a boolean when supplied"))),
    }
}

/// A string list given as an array or as one comma separated string.
fn opt_list(params: &Value, key: &str) -> Result<Option<Vec<String>>, Failure> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(
            text.split(',')
                .map(|part| part.trim().to_string())
                .filter(|part| !part.is_empty())
                .collect(),
        )),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(|text| text.trim().to_string())
                    .ok_or_else(|| invalid(format!("'{key}' must hold strings")))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(invalid(format!(
            "'{key}' must be a list of strings or one comma separated string"
        ))),
    }
}

fn object_type_name(number: i32) -> String {
    al_bc::snapshot::archive::ObjectType::from_i8(number as i8).display_name()
}

fn project_root(workspace: &Workspace) -> Result<PathBuf, Failure> {
    super::project_root_with_wait(workspace)
        .map_err(internal)?
        .ok_or_else(|| invalid("No active project"))
}

/// The debug configuration `name` (the first one when absent) of the project,
/// with the name it resolved to.
fn load_config(root: &Path, name: Option<&str>) -> Result<(BcDebugConfig, String), Failure> {
    let file = al_bc::launch::find_launch_config(root)
        .map_err(|error| invalid(error.to_string()))?
        .ok_or_else(|| {
            invalid(
                "No debug configuration found in project (.zed/debug.json or .vscode/launch.json)",
            )
        })?;
    let picked = pick_named_config(&file.configs, name).map_err(invalid)?;
    let config = debug_config_from_server_config(picked);
    config.validate_native().map_err(invalid)?;
    Ok((config, picked.name.clone()))
}

fn http_client(config: &BcDebugConfig) -> Result<reqwest::Client, Failure> {
    reqwest::Client::builder()
        .danger_accept_invalid_certs(config.accept_invalid_certs)
        .build()
        .map_err(|error| internal(format!("Cannot create the HTTP client: {error}")))
}

async fn token_for(
    workspace: &Workspace,
    config: &BcDebugConfig,
    params: &Value,
) -> Result<String, Failure> {
    let supplied = opt_str(params, "accessToken")?.unwrap_or_default();
    acquire_bc_token(
        workspace,
        config,
        supplied,
        al_project::trust::TargetSource::Repository,
    )
    .await
}

fn origin_text(source: &SnapSource) -> String {
    match source {
        SnapSource::WorkspaceFile(path) => format!("workspace file {}", path.display()),
        SnapSource::PackageSource {
            package,
            publisher,
            version,
            ..
        } => {
            let mut text = format!("package source {package}");
            if let Some(publisher) = publisher {
                text.push_str(&format!(" by {publisher}"));
            }
            if let Some(version) = version {
                text.push_str(&format!(" {version}"));
            }
            text
        }
        SnapSource::ObjectId => "object id".to_string(),
    }
}

fn snappoints_json(points: &[ResolvedSnappoint]) -> Value {
    Value::Array(
        points
            .iter()
            .map(|point| {
                json!({
                    "objectType": object_type_name(point.object_type),
                    "id": point.object_id,
                    "line": u32::from(point.line0) + 1,
                    "condition": point.condition,
                    "origin": origin_text(&point.source),
                })
            })
            .collect(),
    )
}

async fn start(workspace: &Workspace, params: &Value) -> Result<Value, Failure> {
    let root = project_root(workspace)?;
    let allow_production = opt_bool(params, "allowProduction")?;
    let client_text = opt_str(params, "breakOnNext")?.ok_or_else(|| {
        invalid("Missing 'breakOnNext' (WebClient, WebServiceClient, Background or ClientService)")
    })?;
    let (_, client_name) = parse_client_type(client_text).map_err(invalid)?;
    // SnapPoint is the default, as in Microsoft's snapshot debugging.
    let verbosity_name = match opt_str(params, "verbosity")? {
        None => "SnapPoint",
        Some(text) => parse_verbosity(text).map_err(invalid)?.1,
    };
    let session_id = match params.get("sessionId") {
        None | Some(Value::Null) => -1,
        Some(value) => value
            .as_i64()
            .filter(|n| *n >= 0)
            .ok_or_else(|| invalid("'sessionId' must be a non negative integer"))?,
    };
    // Snappoints are optional: SnapPoint verbosity also records a method
    // context that holds an exception.
    let specs = match params.get("snaps") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .ok_or_else(|| invalid("'snaps' must hold strings"))
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err(invalid("'snaps' must be a list of strings")),
    };
    let user = opt_str(params, "userId")?;

    let (config, config_name) = load_config(&root, opt_str(params, "config")?)?;
    guard_production(&config, allow_production).map_err(snapshot_failure)?;

    let mut points = Vec::new();
    for text in specs {
        let spec =
            parse_snap_spec(text).map_err(|error| invalid(format!("snap `{text}`: {error}")))?;
        points.push(
            resolve_snappoint(&workspace.file_index, &root, &spec)
                .map_err(|error| invalid(format!("snap `{text}`: {error}")))?,
        );
    }
    let snappoints = points
        .iter()
        .map(|point| SnappointRecord {
            object_type: point.object_type,
            object_id: point.object_id,
            line0: point.line0,
            condition: point.condition.clone(),
        })
        .collect::<Vec<_>>();

    let store = session_store()?;
    let token = token_for(workspace, &config, params).await?;
    let http = http_client(&config)?;
    let user_id = match user {
        Some(user) => Some(
            snapshot_capture::resolve_user(
                &http,
                &config,
                &token,
                automation_base_override().as_deref(),
                user,
            )
            .await
            .map_err(capture_failure)?,
        ),
        None => None,
    };

    let started = snapshot_capture::start(
        &http,
        &token,
        &store,
        StartRequest {
            config: &config,
            configuration_name: Some(config_name),
            project_root: &root,
            client_type: client_name,
            verbosity: verbosity_name,
            session_id,
            user_id,
            snappoints,
            allow_production,
        },
    )
    .await
    .map_err(capture_failure)?;

    let mut warnings = Vec::new();
    warnings.extend(started.save_error);
    if !points.is_empty() {
        let info = VersionInfo {
            package_version: points.iter().find_map(|point| match &point.source {
                SnapSource::PackageSource { version, .. } => version.clone(),
                _ => None,
            }),
            alpackages_base_app: alpackages_app_version(&root, "Microsoft", "Base Application"),
        };
        warnings.extend(
            environment_version_warnings(
                &http,
                &config,
                &token,
                automation_base_override().as_deref(),
                &info,
            )
            .await,
        );
    }

    Ok(json!({
        "cmd": "start",
        "debuggingContext": started.record.debugging_context,
        "attachKind": started.attach_kind,
        "snappoints": snappoints_json(&points),
        "warnings": warnings,
    }))
}

/// The record named by `context`, or the latest one.
fn pick_record(params: &Value) -> Result<SessionRecord, Failure> {
    let store = session_store()?;
    match opt_str(params, "context")? {
        Some(context) => store
            .load(context)
            .map_err(|error| invalid(format!("context `{context}`: {error}")))?
            .ok_or_else(|| {
                invalid(format!(
                    "no remembered snapshot session for context `{context}`"
                ))
            }),
        None => store
            .latest()
            .map_err(|error| internal(format!("cannot read the snapshot session store: {error}")))?
            .ok_or_else(|| invalid("no remembered snapshot session: run `snapshot start` first")),
    }
}

/// Re-read the project's configuration the session was started with and
/// refuse when it now points somewhere else.
fn config_for_record(
    workspace: &Workspace,
    record: &SessionRecord,
) -> Result<(PathBuf, BcDebugConfig), Failure> {
    let root = project_root(workspace)?;
    let same_project = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    };
    if !same_project(&root, &record.project_root) {
        return Err(invalid(format!(
            "this session was started in project {}, run the command from there",
            record.project_root.display()
        )));
    }
    let (config, _) = load_config(&root, record.configuration_name.as_deref())?;
    let differs = !config.tenant.eq_ignore_ascii_case(&record.tenant)
        || config.environment_name != record.environment_name
        || !config
            .environment_type
            .eq_ignore_ascii_case(&record.environment_type)
        || config.server != record.server
        || config.server_instance != record.server_instance;
    if differs {
        return Err(invalid(
            "the debug configuration's tenant or environment changed since the session started, so it is not used for this session",
        ));
    }
    Ok((root, config))
}

async fn status(workspace: &Workspace, params: &Value) -> Result<Value, Failure> {
    let allow_production = opt_bool(params, "allowProduction")?;
    let mut record = pick_record(params)?;
    let (_, config) = config_for_record(workspace, &record)?;
    guard_production(&config, allow_production).map_err(snapshot_failure)?;
    let store = session_store()?;
    let token = token_for(workspace, &config, params).await?;
    let http = http_client(&config)?;
    let outcome = snapshot_capture::status(
        &http,
        &token,
        &store,
        &config,
        allow_production,
        &mut record,
    )
    .await
    .map_err(capture_failure)?;
    let mut warnings = Vec::new();
    warnings.extend(outcome.save_error);
    warnings.extend(outcome.warning);
    Ok(json!({
        "cmd": "status",
        "debuggingContext": record.debugging_context,
        "status": outcome.status,
        "secondsSinceStarted": outcome.seconds_since_started,
        "warnings": warnings,
    }))
}

/// An explicit `outputDir` resolved inside the project. `None` means the
/// default `<project>/.snapshots`, which the shared finish contains the same way.
fn output_dir(workspace: &Workspace, params: &Value) -> Result<Option<PathBuf>, Failure> {
    match opt_str(params, "outputDir")? {
        Some(dir) => {
            let dir = PathBuf::from(dir);
            if !dir.is_absolute() {
                return Err(invalid("'outputDir' must be an absolute path"));
            }
            resolve_write_param_within_project(workspace, "outputDir", &dir)
                .map(Some)
                .map_err(|rejection| (rejection.code, rejection.message))
        }
        None => Ok(None),
    }
}

async fn finish(workspace: &Workspace, params: &Value) -> Result<Value, Failure> {
    let allow_production = opt_bool(params, "allowProduction")?;
    let dir = output_dir(workspace, params)?;
    let mut record = pick_record(params)?;
    let (_, config) = config_for_record(workspace, &record)?;
    guard_production(&config, allow_production).map_err(snapshot_failure)?;
    let store = session_store()?;
    let token = token_for(workspace, &config, params).await?;
    let http = http_client(&config)?;
    let outcome = snapshot_capture::finish(
        &http,
        &token,
        &store,
        &config,
        allow_production,
        &mut record,
        dir.as_deref(),
    )
    .await
    .map_err(capture_failure)?;
    let mut warnings = Vec::new();
    warnings.extend(outcome.save_error);
    Ok(match outcome.result {
        FinishResult::Empty => json!({
            "cmd": "finish",
            "debuggingContext": record.debugging_context,
            "zipPath": null,
            "bytes": 0,
            "cpuProfilePath": null,
            "empty": true,
            "message": "The server returned no snapshot: nothing hit a snappoint, or no matching session ran.",
        }),
        FinishResult::Saved {
            zip,
            bytes,
            cpu_profile,
        } => json!({
            "cmd": "finish",
            "debuggingContext": record.debugging_context,
            "zipPath": zip.display().to_string(),
            "bytes": bytes,
            "cpuProfilePath": cpu_profile.map(|p| p.display().to_string()),
            "empty": false,
            "warnings": warnings,
        }),
    })
}

fn list() -> Result<Value, Failure> {
    let records = session_store()?
        .list()
        .map_err(|error| internal(format!("cannot read the snapshot session store: {error}")))?;
    let sessions = records
        .iter()
        .map(|record| {
            let mut value = serde_json::to_value(record).unwrap_or(Value::Null);
            if let Some(object) = value.as_object_mut() {
                object.remove("affinityCookie");
            }
            value
        })
        .collect::<Vec<_>>();
    Ok(json!({ "cmd": "list", "sessions": sessions }))
}

async fn show(workspace: &Workspace, params: &Value) -> Result<Value, Failure> {
    let path =
        opt_str(params, "path")?.ok_or_else(|| invalid("Missing 'path' of the snapshot zip"))?;
    let path = resolve_param_within_project(workspace, "path", Path::new(path))
        .map_err(|rejection| (rejection.code, rejection.message))?;
    let only_stop = match params.get("stop") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_u64()
                .filter(|n| *n >= 1)
                .ok_or_else(|| invalid("'stop' must be a positive integer"))? as usize,
        ),
    };
    let options = RenderOptions {
        vars: opt_list(params, "vars")?,
        fields: opt_list(params, "fields")?.unwrap_or_default(),
        only_stop,
    };
    let as_json = opt_bool(params, "json")?;
    tokio::task::spawn_blocking(move || {
        let archive = SnapshotArchive::open(&path)
            .map_err(|error| invalid(format!("cannot read {}: {error}", path.display())))?;
        let stops = archive.stops();
        if as_json {
            let shown: Vec<_> = stops
                .iter()
                .enumerate()
                .filter(|(i, _)| options.only_stop.is_none_or(|n| n == i + 1))
                .map(|(_, stop)| stop)
                .collect();
            Ok(json!({ "cmd": "show", "stops": shown }))
        } else {
            Ok(json!({ "cmd": "show", "text": render_stops(&stops, &options) }))
        }
    })
    .await
    .map_err(|error| internal(format!("snapshot reader stopped: {error}")))?
}

pub(super) async fn dispatch_snapshot(workspace: &Workspace, id: u64, params: &Value) -> Response {
    let cmd = match params.get("cmd").and_then(Value::as_str) {
        Some(cmd) => cmd,
        None => {
            return Response::error(
                id,
                error_codes::INVALID_PARAMS,
                "Missing 'cmd' parameter (expected: start, status, finish, list, show)",
            )
        }
    };
    let outcome = match cmd {
        "start" => start(workspace, params).await,
        "status" => status(workspace, params).await,
        "finish" => finish(workspace, params).await,
        "list" => list(),
        "show" => show(workspace, params).await,
        other => Err(invalid(format!(
            "Unknown snapshot command: {other} (expected: start, status, finish, list, show)"
        ))),
    };
    answer(id, outcome)
}

#[cfg(test)]
#[path = "snapshot_dispatch_tests.rs"]
mod tests;
