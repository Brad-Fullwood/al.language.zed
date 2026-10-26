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
use al_bc::snapshot::state::{now_secs, SessionRecord, SessionStore, SnappointRecord};
use al_dap::dap::bc_debug::automation;
use al_dap::dap::bc_debug::snapshot_debugger::{
    guard_production, new_debugging_context, snapshot_attach, snapshot_finish, snapshot_status,
    SnapshotAttachKind, SnapshotAttachRequest, SnapshotBreakpointLocation, SnapshotClientType,
    SnapshotDebuggerError, SnapshotLine, SnapshotObjectType, SnapshotVerbosity,
};
use al_dap::dap::bc_debug::BcDebugConfig;
use al_protocol::jsonrpc::{error_codes, Response};
use al_workspace::Workspace;
use serde_json::{json, Value};

use super::containment::{resolve_param_within_project, resolve_write_param_within_project};
use super::debug_dispatch::{acquire_bc_token, debug_config_from_server_config, pick_named_config};
use crate::server::dap_mode::snappoints::{
    alpackages_app_version, group_snappoints, parse_snap_spec, resolve_snappoint, version_warning,
    ResolvedSnappoint, SnapSource,
};

/// The server drops a started session that is not finished after this long.
const START_TIMEOUT_SECS: u64 = 600;
/// Warn from this long after the session started.
const START_WARN_SECS: u64 = 480;

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

fn client_type(text: &str) -> Result<(SnapshotClientType, &'static str), Failure> {
    let normalised = text.replace([' ', '-', '_'], "").to_ascii_lowercase();
    match normalised.as_str() {
        "webclient" => Ok((SnapshotClientType::WebClient, "WebClient")),
        "webserviceclient" => Ok((SnapshotClientType::WebServiceClient, "WebServiceClient")),
        "background" => Ok((SnapshotClientType::Background, "Background")),
        "clientservice" => Ok((SnapshotClientType::ClientService, "ClientService")),
        _ => Err(invalid(format!(
            "breakOnNext must be one of WebClient, WebServiceClient, Background, ClientService; got `{text}`"
        ))),
    }
}

fn object_type(number: i32) -> Result<SnapshotObjectType, Failure> {
    Ok(match number {
        1 => SnapshotObjectType::Table,
        3 => SnapshotObjectType::Report,
        5 => SnapshotObjectType::CodeUnit,
        6 => SnapshotObjectType::XmlPort,
        8 => SnapshotObjectType::Page,
        9 => SnapshotObjectType::Query,
        14 => SnapshotObjectType::PageExtension,
        15 => SnapshotObjectType::TableExtension,
        16 => SnapshotObjectType::Enum,
        17 => SnapshotObjectType::EnumExtension,
        22 => SnapshotObjectType::ReportExtension,
        28 => SnapshotObjectType::Interface,
        other => {
            return Err(invalid(format!(
                "object type {other} cannot take a snappoint"
            )))
        }
    })
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

/// The Base Application version of the environment, as a warning text list
/// entry when it differs from where the snappoints were set. A failed lookup
/// is a warning, not an error.
async fn version_warnings(
    http: &reqwest::Client,
    config: &BcDebugConfig,
    token: &str,
    root: &Path,
    points: &[ResolvedSnappoint],
) -> Vec<String> {
    let lookup = match automation_base_override() {
        Some(base) => automation::base_app_version_at(http, &base, token).await,
        None => automation::base_application_version(http, config, token).await,
    };
    match lookup {
        Ok(environment) => {
            let package_version = points.iter().find_map(|point| match &point.source {
                SnapSource::PackageSource { version, .. } => version.clone(),
                _ => None,
            });
            let alpackages = alpackages_app_version(root, "Microsoft", "Base Application");
            version_warning(
                &environment,
                package_version.as_deref(),
                alpackages.as_deref(),
            )
            .into_iter()
            .collect()
        }
        Err(error) => vec![format!("Base Application version not checked: {error}")],
    }
}

async fn resolve_user(
    http: &reqwest::Client,
    config: &BcDebugConfig,
    token: &str,
    user: &str,
) -> Result<String, Failure> {
    if al_bc::bc_client::is_guid(user) {
        return Ok(user.to_string());
    }
    let resolved = match automation_base_override() {
        Some(base) => automation::resolve_user_at(http, &base, token, user).await,
        None => automation::resolve_user_security_id(http, config, token, user).await,
    };
    resolved.map_err(|error| invalid(format!("userId `{user}`: {error}")))
}

async fn start(workspace: &Workspace, params: &Value) -> Result<Value, Failure> {
    let root = project_root(workspace)?;
    let allow_production = opt_bool(params, "allowProduction")?;
    let (client, client_name) = client_type(opt_str(params, "breakOnNext")?.ok_or_else(|| {
        invalid("Missing 'breakOnNext' (WebClient, WebServiceClient, Background or ClientService)")
    })?)?;
    let verbosity = match opt_str(params, "verbosity")?
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        None | Some("full") => (SnapshotVerbosity::Full, "Full"),
        Some("snappoint") => (SnapshotVerbosity::SnapPoint, "SnapPoint"),
        Some(other) => {
            return Err(invalid(format!(
                "verbosity must be snappoint or full; got `{other}`"
            )))
        }
    };
    let session_id = match params.get("sessionId") {
        None | Some(Value::Null) => -1,
        Some(value) => value
            .as_i64()
            .filter(|n| *n >= 0)
            .ok_or_else(|| invalid("'sessionId' must be a non negative integer"))?,
    };
    let specs = match params.get("snaps") {
        Some(Value::Array(items)) if !items.is_empty() => items
            .iter()
            .map(|item| {
                item.as_str()
                    .ok_or_else(|| invalid("'snaps' must hold strings"))
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(invalid(
            "Missing 'snaps': a list of snappoints such as `codeunit:22:176?cond` or `file.al:120`",
        )),
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
    let groups = group_snappoints(&points);
    let locations = groups
        .iter()
        .map(|group| {
            Ok(SnapshotBreakpointLocation {
                object_type: object_type(group.object_type)?,
                object_number: u32::try_from(group.object_id).map_err(|_| {
                    invalid(format!("object id {} is out of range", group.object_id))
                })?,
                lines: group
                    .lines
                    .iter()
                    .map(|line| SnapshotLine {
                        line: line.line0,
                        condition: line.condition.clone().unwrap_or_default(),
                    })
                    .collect(),
            })
        })
        .collect::<Result<Vec<_>, Failure>>()?;

    let token = token_for(workspace, &config, params).await?;
    let http = http_client(&config)?;
    let user_id = match user {
        Some(user) => Some(resolve_user(&http, &config, &token, user).await?),
        None => None,
    };

    let debugging_context = new_debugging_context();
    let mut request = SnapshotAttachRequest::new(debugging_context.clone());
    request.client_type = client;
    request.user_id = user_id.clone();
    request.locations = locations;
    request.verbosity = verbosity.0;
    request.session_id = session_id;
    let attached = snapshot_attach(&http, &config, &token, allow_production, &request)
        .await
        .map_err(snapshot_failure)?;
    let attach_kind = match attached.kind {
        SnapshotAttachKind::Undefined => "Undefined",
        SnapshotAttachKind::UserSession => "UserSession",
        SnapshotAttachKind::NextSessionForUserOnTenant => "NextSessionForUserOnTenant",
        SnapshotAttachKind::NextSessionOnTenant => "NextSessionOnTenant",
    };

    let record = SessionRecord {
        debugging_context: debugging_context.clone(),
        affinity_cookie: attached.affinity_cookie,
        attach_kind: attach_kind.to_string(),
        created_at: now_secs(),
        project_root: root.clone(),
        configuration_name: Some(config_name),
        tenant: config.tenant.clone(),
        environment_type: config.environment_type.clone(),
        environment_name: config.environment_name.clone(),
        server: config.server.clone(),
        server_instance: config.server_instance.clone(),
        snapshot_port: config.snapshot_port,
        client_type: client_name.to_string(),
        user_id,
        session_id,
        verbosity: verbosity.1.to_string(),
        snappoints: points
            .iter()
            .map(|point| SnappointRecord {
                object_type: point.object_type,
                object_id: point.object_id,
                line0: point.line0,
                condition: point.condition.clone(),
            })
            .collect(),
        last_status: None,
        last_status_at: None,
        started_at: None,
        zip_path: None,
        finished_at: None,
    };
    let mut warnings = Vec::new();
    if let Err(error) = session_store()?.save(&record) {
        warnings.push(format!(
            "The session could not be saved, so status and finish need the context passed explicitly and will not find it: {error}"
        ));
    }
    if config.environment_type.eq_ignore_ascii_case("OnPrem")
        && automation_base_override().is_none()
    {
        warnings.push(
            "Base Application version not checked: the lookup needs a SaaS environment".to_string(),
        );
    } else {
        warnings.extend(version_warnings(&http, &config, &token, &root, &points).await);
    }

    Ok(json!({
        "cmd": "start",
        "debuggingContext": debugging_context,
        "attachKind": attach_kind,
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
    let token = token_for(workspace, &config, params).await?;
    let http = http_client(&config)?;
    let result = snapshot_status(
        &http,
        &config,
        &token,
        allow_production,
        &record.debugging_context,
        record.affinity_cookie.as_deref(),
    )
    .await
    .map_err(snapshot_failure)?;
    let now = now_secs();
    let status = result.status.to_string();
    record.note_status(&status, now);
    let mut warnings = Vec::new();
    if let Err(error) = session_store()?.save(&record) {
        warnings.push(format!("The session could not be updated: {error}"));
    }
    let elapsed = record.seconds_since_started(now);
    if let Some(seconds) = elapsed {
        if status == "Started" && seconds >= START_WARN_SECS {
            let left = START_TIMEOUT_SECS.saturating_sub(seconds);
            warnings.push(format!(
                "The session started {seconds} seconds ago. The server drops a started session after 10 minutes ({left} seconds left), so finish it now."
            ));
        }
    }
    Ok(json!({
        "cmd": "status",
        "debuggingContext": record.debugging_context,
        "status": status,
        "secondsSinceStarted": elapsed,
        "warnings": warnings,
    }))
}

/// `outputDir` resolved inside the project, or `<project>/.snapshots`.
fn output_dir(workspace: &Workspace, params: &Value) -> Result<PathBuf, Failure> {
    match opt_str(params, "outputDir")? {
        Some(dir) => {
            let dir = PathBuf::from(dir);
            if !dir.is_absolute() {
                return Err(invalid("'outputDir' must be an absolute path"));
            }
            resolve_write_param_within_project(workspace, "outputDir", &dir)
                .map_err(|rejection| (rejection.code, rejection.message))
        }
        None => Ok(project_root(workspace)?.join(".snapshots")),
    }
}

async fn finish(workspace: &Workspace, params: &Value) -> Result<Value, Failure> {
    let allow_production = opt_bool(params, "allowProduction")?;
    let dir = output_dir(workspace, params)?;
    let mut record = pick_record(params)?;
    let (_, config) = config_for_record(workspace, &record)?;
    guard_production(&config, allow_production).map_err(snapshot_failure)?;
    let token = token_for(workspace, &config, params).await?;
    let http = http_client(&config)?;
    let finished = snapshot_finish(
        &http,
        &config,
        &token,
        allow_production,
        &record.debugging_context,
        record.affinity_cookie.as_deref(),
    )
    .await
    .map_err(snapshot_failure)?;
    let now = now_secs();
    record.finished_at = Some(now);
    let Some(zip) = finished.zip else {
        record.last_status = Some("Finished".to_string());
        record.last_status_at = Some(now);
        let _ = session_store()?.save(&record);
        return Ok(json!({
            "cmd": "finish",
            "debuggingContext": record.debugging_context,
            "zipPath": null,
            "bytes": 0,
            "cpuProfilePath": null,
            "empty": true,
            "message": "The server returned no snapshot: nothing hit a snappoint, or no matching session ran.",
        }));
    };
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|error| internal(format!("cannot create {}: {error}", dir.display())))?;
    let bytes = zip.len();
    let saved = al_bc::snapshot::save_finished_snapshot(
        &dir,
        &record.debugging_context,
        zip,
        finished.sampling,
    )
    .await
    .map_err(|error| internal(format!("cannot save the snapshot: {error}")))?;
    record.zip_path = Some(saved.zip.clone());
    record.last_status = Some("Finished".to_string());
    record.last_status_at = Some(now);
    let mut warnings = Vec::new();
    if let Err(error) = session_store()?.save(&record) {
        warnings.push(format!("The session could not be updated: {error}"));
    }
    Ok(json!({
        "cmd": "finish",
        "debuggingContext": record.debugging_context,
        "zipPath": saved.zip.display().to_string(),
        "bytes": bytes,
        "cpuProfilePath": saved.cpu_profile.map(|p| p.display().to_string()),
        "empty": false,
        "warnings": warnings,
    }))
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
