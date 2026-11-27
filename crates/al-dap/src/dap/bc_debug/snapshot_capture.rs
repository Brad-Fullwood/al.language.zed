//! Snapshot capture: the steps the daemon's `snapshot` method and the native
//! DAP's `attach` with `"snapshot": "capture"` share.
//!
//! `start` groups the snappoints, calls the attach endpoint and saves a
//! `SessionRecord`. `status` polls and updates that record. `finish` downloads
//! the zip into a contained output directory and updates the record again. The
//! callers decide how to authorise, where tokens come from and how results are
//! shown. Nothing here reads a token from anywhere but its arguments, and the
//! record never holds one.
//!
//! Routes checked before writing this: the REST calls are the ones in
//! `snapshot_debugger`, the zip write is `al_bc::snapshot::save_finished_snapshot`
//! and the `.gitignore` is written with `al_bc::output_file::write_no_follow`.
//! The project containment helpers (`resolve_output_path_within_project`) are
//! `pub(crate)` in al-lsp, which al-dap cannot depend on, and al-project and
//! al-workspace hold no equivalent, so `resolve_output_dir` below canonicalises
//! the deepest existing ancestor and checks it against the canonical project
//! root.

use std::path::{Component, Path, PathBuf};

use al_bc::snapshot::state::{now_secs, SessionRecord, SessionStore, SnappointRecord};

use super::automation;
use super::snapshot_debugger::{
    new_debugging_context, snapshot_attach, snapshot_endpoint_metadata, snapshot_finish,
    snapshot_status, SnapshotAttachKind, SnapshotAttachRequest, SnapshotBreakpointLocation,
    SnapshotClientType, SnapshotDebuggerError, SnapshotLine, SnapshotObjectType, SnapshotProfile,
    SnapshotProfileKind, SnapshotVerbosity, EXECUTION_CONTEXT_PROFILING,
    SAMPLE_PROFILING_MIN_API_VERSION,
};
use super::BcDebugConfig;

/// The server drops a started session that is not finished after this long.
pub const START_TIMEOUT_SECS: u64 = 600;
/// Warn from this long after the session started.
pub const START_WARN_SECS: u64 = 480;

/// Why a capture step failed. `Invalid` is something the caller asked for,
/// `Internal` is a failure on this machine, `Snapshot` is the REST layer.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Internal(String),
    #[error(transparent)]
    Snapshot(#[from] SnapshotDebuggerError),
}

pub type Result<T> = std::result::Result<T, CaptureError>;

fn invalid(message: impl Into<String>) -> CaptureError {
    CaptureError::Invalid(message.into())
}

fn internal(message: impl Into<String>) -> CaptureError {
    CaptureError::Internal(message.into())
}

/// The `breakOnNext` choice: the wire value and its canonical name.
pub fn parse_client_type(
    text: &str,
) -> std::result::Result<(SnapshotClientType, &'static str), String> {
    let normalised = text.replace([' ', '-', '_'], "").to_ascii_lowercase();
    match normalised.as_str() {
        "webclient" => Ok((SnapshotClientType::WebClient, "WebClient")),
        "webserviceclient" => Ok((SnapshotClientType::WebServiceClient, "WebServiceClient")),
        "background" => Ok((SnapshotClientType::Background, "Background")),
        "clientservice" => Ok((SnapshotClientType::ClientService, "ClientService")),
        _ => Err(format!(
            "breakOnNext must be one of WebClient, WebServiceClient, Background, ClientService; got `{text}`"
        )),
    }
}

/// `snappoint` or `full`, any case: the wire value and its canonical name.
pub fn parse_verbosity(
    text: &str,
) -> std::result::Result<(SnapshotVerbosity, &'static str), String> {
    match text.to_ascii_lowercase().as_str() {
        "full" => Ok((SnapshotVerbosity::Full, "Full")),
        "snappoint" => Ok((SnapshotVerbosity::SnapPoint, "SnapPoint")),
        _ => Err(format!("verbosity must be snappoint or full; got `{text}`")),
    }
}

/// The wire object type for a BC object type number.
pub fn object_type(number: i32) -> Result<SnapshotObjectType> {
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

/// Group by `(object type, object id)` in first-seen order, lines in
/// first-seen order, identical `(line, condition)` pairs once. A missing
/// condition is the empty string, which the attach body omits.
pub fn group_snappoints(points: &[SnappointRecord]) -> Result<Vec<SnapshotBreakpointLocation>> {
    let mut groups: Vec<(i32, i32, Vec<SnapshotLine>)> = Vec::new();
    for point in points {
        let line = SnapshotLine {
            line: point.line0,
            condition: point.condition.clone().unwrap_or_default(),
        };
        match groups
            .iter_mut()
            .find(|(kind, id, _)| *kind == point.object_type && *id == point.object_id)
        {
            Some((_, _, lines)) => {
                if !lines.contains(&line) {
                    lines.push(line);
                }
            }
            None => groups.push((point.object_type, point.object_id, vec![line])),
        }
    }
    groups
        .into_iter()
        .map(|(kind, id, lines)| {
            Ok(SnapshotBreakpointLocation {
                object_type: object_type(kind)?,
                object_number: u32::try_from(id)
                    .map_err(|_| invalid(format!("object id {id} is out of range")))?,
                lines,
            })
        })
        .collect()
}

/// Dotted version split into numbers, a part that is not a number counting 0.
pub fn version_key(version: &str) -> Vec<u64> {
    version
        .split('.')
        .map(|part| part.trim().parse().unwrap_or(0))
        .collect()
}

/// Equal after dropping trailing zero components, so `27.1` equals `27.1.0.0`.
fn same_version(a: &str, b: &str) -> bool {
    let trim = |mut v: Vec<u64>| {
        while v.last() == Some(&0) {
            v.pop();
        }
        v
    };
    trim(version_key(a)) == trim(version_key(b))
}

/// A warning when a version the snappoints were set against differs from the
/// environment's BaseApp version. `package_version` is the version of the
/// package a snappoint's source came from. `alpackages_base_app` is the
/// BaseApp version in `.alpackages`. Either may be absent.
pub fn version_warning(
    environment_base_app: &str,
    package_version: Option<&str>,
    alpackages_base_app: Option<&str>,
) -> Option<String> {
    let environment = environment_base_app.trim();
    let mut notes = Vec::new();
    for (label, version) in [
        ("the package the snappoint came from", package_version),
        ("the .alpackages BaseApp", alpackages_base_app),
    ] {
        if let Some(version) = version.map(str::trim) {
            if !same_version(version, environment) {
                notes.push(format!("{label} is {version}"));
            }
        }
    }
    if notes.is_empty() {
        return None;
    }
    Some(format!(
        "The environment runs BaseApp {environment} but {}. Line numbers can differ from the code the server runs, so a snappoint may land on the wrong line.",
        notes.join(" and ")
    ))
}

/// The versions the snappoints were set against.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VersionInfo {
    /// Version of the package a snappoint's source came from.
    pub package_version: Option<String>,
    /// BaseApp version in the project's `.alpackages`.
    pub alpackages_base_app: Option<String>,
}

/// Compare the environment's BaseApp version with `info`. A failed lookup is a
/// warning, not an error. `automation_base` replaces the Automation API base
/// derived from the configuration, for a proxy or a test server. Without it an
/// OnPrem environment is not checked, because the lookup needs the SaaS API.
pub async fn environment_version_warnings(
    http: &reqwest::Client,
    config: &BcDebugConfig,
    token: &str,
    automation_base: Option<&str>,
    info: &VersionInfo,
) -> Vec<String> {
    if automation_base.is_none() && config.environment_type.eq_ignore_ascii_case("OnPrem") {
        return vec![
            "Base Application version not checked: the lookup needs a SaaS environment".to_string(),
        ];
    }
    let lookup = match automation_base {
        Some(base) => automation::base_app_version_at(http, base, token).await,
        None => automation::base_application_version(http, config, token).await,
    };
    match lookup {
        Ok(environment) => version_warning(
            &environment,
            info.package_version.as_deref(),
            info.alpackages_base_app.as_deref(),
        )
        .into_iter()
        .collect(),
        Err(error) => vec![format!("Base Application version not checked: {error}")],
    }
}

/// A user name resolved to its `userSecurityId`. A GUID is used as it is.
pub async fn resolve_user(
    http: &reqwest::Client,
    config: &BcDebugConfig,
    token: &str,
    automation_base: Option<&str>,
    user: &str,
) -> Result<String> {
    if al_bc::bc_client::is_guid(user) {
        return Ok(user.to_string());
    }
    let resolved = match automation_base {
        Some(base) => automation::resolve_user_at(http, base, token, user).await,
        None => automation::resolve_user_security_id(http, config, token, user).await,
    };
    resolved.map_err(|error| invalid(format!("userId `{user}`: {error}")))
}

/// What `start` needs besides the connection.
pub struct StartRequest<'a> {
    pub config: &'a BcDebugConfig,
    pub configuration_name: Option<String>,
    pub project_root: &'a Path,
    /// `breakOnNext`, parsed by [`parse_client_type`].
    pub client_type: &'a str,
    /// `SnapPoint` or `Full`, parsed by [`parse_verbosity`].
    pub verbosity: &'a str,
    /// -1 when not chosen by id.
    pub session_id: i64,
    pub user_id: Option<String>,
    pub snappoints: Vec<SnappointRecord>,
    pub allow_production: bool,
    /// Attach for profiling (ExecutionContext 2) instead of debugging.
    pub profile: Option<SnapshotProfile>,
}

pub struct Started {
    pub record: SessionRecord,
    pub attach_kind: &'static str,
    /// The record could not be saved, so a later status or finish will not
    /// find it unless the context is passed explicitly.
    pub save_error: Option<String>,
}

fn attach_kind_text(kind: SnapshotAttachKind) -> &'static str {
    match kind {
        SnapshotAttachKind::Undefined => "Undefined",
        SnapshotAttachKind::UserSession => "UserSession",
        SnapshotAttachKind::NextSessionForUserOnTenant => "NextSessionForUserOnTenant",
        SnapshotAttachKind::NextSessionOnTenant => "NextSessionOnTenant",
    }
}

/// Sampling profiling needs `webApiVersion` 3.0. Mirrors
/// SnapshotDebuggerAttachInitializeRequestHandler.cs:60-68: the attach is refused
/// only when the server reported a version below 3.0, and goes ahead when the
/// metadata is unavailable (the host's `serverInfo != null` test).
async fn ensure_sampling_supported(
    http: &reqwest::Client,
    config: &BcDebugConfig,
    token: &str,
    allow_production: bool,
) -> Result<()> {
    let Ok((metadata, _)) = snapshot_endpoint_metadata(http, config, token, allow_production).await
    else {
        return Ok(());
    };
    let Some(version) = metadata.web_api_version.as_deref() else {
        return Ok(());
    };
    let (mut have, mut need) = (
        version_key(version),
        version_key(SAMPLE_PROFILING_MIN_API_VERSION),
    );
    let width = have.len().max(need.len());
    have.resize(width, 0);
    need.resize(width, 0);
    if have < need {
        return Err(invalid(format!(
            "sampling profiling needs snapshot API version {SAMPLE_PROFILING_MIN_API_VERSION} or later, this environment reports {version}. Use --kind instrumentation"
        )));
    }
    Ok(())
}

/// Attach the snappoints and remember the session. With `profile` set it
/// attaches a profiling session (no snappoints) instead.
pub async fn start(
    http: &reqwest::Client,
    token: &str,
    store: &SessionStore,
    request: StartRequest<'_>,
) -> Result<Started> {
    // No snappoints is valid: SnapPoint verbosity also records a method context
    // that holds an exception (Microsoft Learn, snapshot debugging), and the
    // attach body then carries an empty SourceBreakpointLocations list.
    let (client, client_name) = parse_client_type(request.client_type).map_err(invalid)?;
    let (verbosity, verbosity_name) = parse_verbosity(request.verbosity).map_err(invalid)?;
    let locations = group_snappoints(&request.snappoints)?;

    if matches!(
        request.profile,
        Some(SnapshotProfile {
            kind: SnapshotProfileKind::Sampling,
            ..
        })
    ) {
        ensure_sampling_supported(http, request.config, token, request.allow_production).await?;
    }

    let debugging_context = new_debugging_context();
    let mut attach = SnapshotAttachRequest::new(debugging_context.clone());
    if let Some(profile) = request.profile {
        attach.execution_context = EXECUTION_CONTEXT_PROFILING;
        attach.profile = Some(profile);
    }
    attach.client_type = client;
    attach.user_id = request.user_id.clone();
    attach.locations = locations;
    attach.verbosity = verbosity;
    attach.session_id = request.session_id;
    let attached = snapshot_attach(
        http,
        request.config,
        token,
        request.allow_production,
        &attach,
    )
    .await?;
    let attach_kind = attach_kind_text(attached.kind);

    let config = request.config;
    let record = SessionRecord {
        debugging_context,
        affinity_cookie: attached.affinity_cookie,
        attach_kind: attach_kind.to_string(),
        created_at: now_secs(),
        project_root: request.project_root.to_path_buf(),
        configuration_name: request.configuration_name,
        tenant: config.tenant.clone(),
        environment_type: config.environment_type.clone(),
        environment_name: config.environment_name.clone(),
        server: config.server.clone(),
        server_instance: config.server_instance.clone(),
        snapshot_port: config.snapshot_port,
        client_type: client_name.to_string(),
        user_id: request.user_id,
        session_id: request.session_id,
        verbosity: verbosity_name.to_string(),
        snappoints: request.snappoints,
        last_status: None,
        last_status_at: None,
        started_at: None,
        zip_path: None,
        finished_at: None,
        profile: request.profile.map(|p| format!("{:?}", p.kind)),
    };
    let save_error = store.save(&record).err().map(|error| {
        format!(
            "The session could not be saved, so status and finish need the context passed explicitly and will not find it: {error}"
        )
    });
    Ok(Started {
        record,
        attach_kind,
        save_error,
    })
}

/// The warning text for a started session that is close to the server's
/// 10 minute limit.
pub fn started_warning(seconds_since_started: u64) -> String {
    let left = START_TIMEOUT_SECS.saturating_sub(seconds_since_started);
    format!(
        "The session started {seconds_since_started} seconds ago. The server drops a started session after 10 minutes ({left} seconds left), so finish it now."
    )
}

pub struct StatusOutcome {
    pub status: String,
    pub seconds_since_started: Option<u64>,
    /// Set while the status is `Started` and 8 minutes or more have passed.
    /// A repeating caller shows it once.
    pub warning: Option<String>,
    pub save_error: Option<String>,
}

/// Poll the server, note the answer in `record` and save it. A finish that
/// another process saved to the store is kept. There is no lock across
/// processes, so a finish saved between the load and the save can still be
/// overwritten.
pub async fn status(
    http: &reqwest::Client,
    token: &str,
    store: &SessionStore,
    config: &BcDebugConfig,
    allow_production: bool,
    record: &mut SessionRecord,
) -> Result<StatusOutcome> {
    let result = snapshot_status(
        http,
        config,
        token,
        allow_production,
        &record.debugging_context,
        record.affinity_cookie.as_deref(),
    )
    .await?;
    let now = now_secs();
    let status = result.status.to_string();
    record.note_status(&status, now);
    if let Ok(Some(stored)) = store.load(&record.debugging_context) {
        if stored.finished_at.is_some() {
            record.finished_at = stored.finished_at;
            record.zip_path = stored.zip_path;
        }
        if record.profile.is_none() {
            record.profile = stored.profile;
        }
    }
    let save_error = store
        .save(record)
        .err()
        .map(|error| format!("The session could not be updated: {error}"));
    let seconds_since_started = record.seconds_since_started(now);
    let warning = seconds_since_started
        .filter(|seconds| status == "Started" && *seconds >= START_WARN_SECS)
        .map(started_warning);
    Ok(StatusOutcome {
        status,
        seconds_since_started,
        warning,
        save_error,
    })
}

/// `requested` (relative to the project root, or absolute), or
/// `<project>/.snapshots`, when it lies inside the project root. The deepest
/// existing ancestor is canonicalised, so a symlink inside the project that
/// points out of it is refused. The directory need not exist yet. No file
/// system change is made.
pub fn resolve_output_dir(project_root: &Path, requested: Option<&Path>) -> Result<PathBuf> {
    let requested = requested.unwrap_or_else(|| Path::new(".snapshots"));
    let escapes = || {
        invalid(format!(
            "snapshot output directory '{}' is outside the project at '{}'",
            requested.display(),
            project_root.display()
        ))
    };
    let root = project_root.canonicalize().map_err(|error| {
        internal(format!(
            "cannot resolve {}: {error}",
            project_root.display()
        ))
    })?;
    let absolute = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        project_root.join(requested)
    };
    let mut normalised = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                if !normalised.pop() {
                    return Err(escapes());
                }
            }
            Component::CurDir => {}
            other => normalised.push(other.as_os_str()),
        }
    }
    let mut existing = normalised.as_path();
    let mut missing = Vec::new();
    let mut resolved = loop {
        match existing.canonicalize() {
            Ok(path) => break path,
            Err(_) => {
                // A dangling symlink cannot be canonicalised. Refuse it rather
                // than treat its name as a directory still to be created.
                if std::fs::symlink_metadata(existing).is_ok_and(|m| m.file_type().is_symlink()) {
                    return Err(escapes());
                }
                missing.push(existing.file_name().ok_or_else(escapes)?);
                existing = existing.parent().ok_or_else(escapes)?;
            }
        }
    };
    for name in missing.iter().rev() {
        resolved.push(name);
    }
    if !resolved.starts_with(&root) {
        return Err(escapes());
    }
    Ok(resolved)
}

/// Check `dir`, the output directory `resolve_output_dir` returned. A missing
/// directory is created and given a `.gitignore` holding `*`, because snapshot
/// zips hold customer data. An existing entry must be a real directory, not a
/// symbolic link or a file. Either way it must resolve to itself under the
/// project root, which catches a link planted after the first check.
async fn prepare_output_dir(dir: &Path, project_root: &Path) -> Result<()> {
    match tokio::fs::symlink_metadata(dir).await {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(invalid(format!(
                    "snapshot output directory '{}' is not a real directory",
                    dir.display()
                )));
            }
            ensure_resolves_to(dir, project_root)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|error| internal(format!("cannot create {}: {error}", dir.display())))?;
            ensure_resolves_to(dir, project_root)?;
            al_bc::output_file::write_no_follow(&dir.join(".gitignore"), b"*\n".to_vec())
                .await
                .map_err(|error| {
                    internal(format!(
                        "cannot write the .gitignore in {}: {error}",
                        dir.display()
                    ))
                })
        }
        Err(error) => Err(internal(format!("cannot read {}: {error}", dir.display()))),
    }
}

/// Refuse `dir` unless `resolve_output_dir` returns it unchanged.
fn ensure_resolves_to(dir: &Path, project_root: &Path) -> Result<()> {
    let resolved = resolve_output_dir(project_root, Some(dir))?;
    if resolved != dir {
        return Err(invalid(format!(
            "snapshot output directory '{}' resolves to '{}', which is not where it was checked",
            dir.display(),
            resolved.display()
        )));
    }
    Ok(())
}

pub enum FinishResult {
    /// The server returned no snapshot: nothing hit a snappoint, or no matching
    /// session ran.
    Empty,
    Saved {
        zip: PathBuf,
        bytes: usize,
        cpu_profile: Option<PathBuf>,
    },
}

pub struct FinishOutcome {
    pub result: FinishResult,
    pub save_error: Option<String>,
}

/// Check the output directory, then save the zip into it.
async fn save_zip(
    dir: &Path,
    record: &SessionRecord,
    zip: Vec<u8>,
    sampling: bool,
) -> Result<al_bc::snapshot::SavedSnapshot> {
    prepare_output_dir(dir, &record.project_root).await?;
    al_bc::snapshot::save_finished_snapshot(dir, &record.debugging_context, zip, sampling)
        .await
        .map_err(|error| internal(format!("cannot save the snapshot: {error}")))
}

/// Append `note` to the text of `error`. `Invalid` stays `Invalid`, because
/// `al-lsp` maps the two kinds to different responses.
fn with_note(error: CaptureError, note: &str) -> CaptureError {
    match error {
        CaptureError::Invalid(text) => invalid(format!("{text}, {note}")),
        other => internal(format!("{other}, {note}")),
    }
}

/// Where `finish` writes the zip when the session store and the output folder
/// fail. Tests replace both fields.
struct FinishPlaces<F> {
    /// Returns the third folder. Called only when the first two writes fail.
    fallback_dir: F,
    /// Writes the zip bytes into the open staging file.
    write_staging: fn(&mut std::fs::File, &[u8]) -> std::io::Result<()>,
}

fn write_and_sync(file: &mut std::fs::File, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    file.write_all(bytes)?;
    file.sync_all()
}

/// `<store dir>/.<context>.zip.partial`, created empty with mode 0600. A stale
/// regular file left by an earlier run is replaced. A link or any other entry
/// at that name is refused.
fn create_staging_file(store: &SessionStore, context: &str) -> Result<(PathBuf, std::fs::File)> {
    let path = store
        .dir()
        .join(format!(".{}.zip.partial", context.to_ascii_lowercase()));
    let fail = |error: &dyn std::fmt::Display| {
        invalid(format!(
            "cannot create the staging file {} in the session store {}: {error}. The capture was not finished.",
            path.display(),
            store.dir().display()
        ))
    };
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(store.dir()).map_err(|e| fail(&e))?;
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_file() => {
            std::fs::remove_file(&path).map_err(|e| fail(&e))?
        }
        Ok(_) => return Err(fail(&"an entry that is not a regular file is in the way")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(fail(&error)),
    }
    let mut options = std::fs::OpenOptions::new();
    // `create_new` refuses any entry at the name, a link included.
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path).map_err(|e| fail(&e))?;
    Ok((path, file))
}

fn remove_staging(path: &Path) -> Option<String> {
    match std::fs::remove_file(path) {
        Ok(()) => None,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => Some(format!(
            "The staging file {} could not be removed: {error}",
            path.display()
        )),
    }
}

/// Save `record` and add a failed save to `note`.
fn save_with_note(store: &SessionStore, record: &SessionRecord, note: String) -> String {
    match store.save(record) {
        Ok(_) => note,
        Err(error) => format!("{note}, and the session could not be updated: {error}"),
    }
}

/// Finish the session, save the zip into `output_dir` (default
/// `<project>/.snapshots`) and update the record. The output directory is
/// checked, and a staging file is created in the session store, before the
/// server is asked to finish. A refused directory or an unwritable store
/// therefore does not cost the snapshot.
///
/// The zip is written to the staging file first, then saved to the output
/// directory. When the output save fails, the staging file is renamed to the
/// store's recovery path. When the staging write failed as well, the zip goes
/// to a private recovery folder on the runtime filesystem
/// ([`al_bc::snapshot::private_recovery_dir`]). The error names the place the
/// zip was kept, or all three places and the byte count when none could be
/// written.
pub async fn finish(
    http: &reqwest::Client,
    token: &str,
    store: &SessionStore,
    config: &BcDebugConfig,
    allow_production: bool,
    record: &mut SessionRecord,
    output_dir: Option<&Path>,
) -> Result<FinishOutcome> {
    let places = FinishPlaces {
        fallback_dir: al_bc::snapshot::private_recovery_dir,
        write_staging: write_and_sync,
    };
    finish_with(
        http,
        token,
        store,
        config,
        allow_production,
        record,
        output_dir,
        places,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn finish_with<F>(
    http: &reqwest::Client,
    token: &str,
    store: &SessionStore,
    config: &BcDebugConfig,
    allow_production: bool,
    record: &mut SessionRecord,
    output_dir: Option<&Path>,
    places: FinishPlaces<F>,
) -> Result<FinishOutcome>
where
    F: FnOnce() -> std::io::Result<PathBuf> + Send,
{
    let dir = resolve_output_dir(&record.project_root, output_dir)?;
    let context = record.debugging_context.to_ascii_lowercase();
    let recovery = store
        .recovery_path(&record.debugging_context)
        .map_err(|error| invalid(error.to_string()))?;
    let (staging, mut staging_file) = create_staging_file(store, &context)?;
    let finished = match snapshot_finish(
        http,
        config,
        token,
        allow_production,
        &record.debugging_context,
        record.affinity_cookie.as_deref(),
    )
    .await
    {
        Ok(finished) => finished,
        Err(error) => {
            drop(staging_file);
            let _ = remove_staging(&staging);
            return Err(error.into());
        }
    };
    let now = now_secs();
    record.finished_at = Some(now);
    record.last_status = Some("Finished".to_string());
    record.last_status_at = Some(now);
    let Some(zip) = finished.zip else {
        drop(staging_file);
        let staging_error = remove_staging(&staging);
        let record_error = store
            .save(record)
            .err()
            .map(|error| format!("The session could not be updated: {error}"));
        return Ok(FinishOutcome {
            result: FinishResult::Empty,
            save_error: record_error.or(staging_error),
        });
    };
    let bytes = zip.len();
    let staged = (places.write_staging)(&mut staging_file, &zip);
    drop(staging_file);
    // The cloud server sends no ETag on finish (recorded from CDX
    // 2026-10-27), so Microsoft's ETag test alone would never extract the
    // profile. The record knows the attach asked for a sampling profile.
    let sampling = finished.sampling || record.profile.as_deref() == Some("Sampling");
    // A copy, so the zip can be kept if the save fails.
    let saved = match save_zip(&dir, record, zip.clone(), sampling).await {
        Ok(saved) => saved,
        Err(error) => {
            return Err(keep_zip(
                store, record, zip, error, staged, &staging, &recovery, &dir, places,
            )
            .await)
        }
    };
    let staging_error = remove_staging(&staging);
    record.zip_path = Some(saved.zip.clone());
    let record_error = store
        .save(record)
        .err()
        .map(|error| format!("The session could not be updated: {error}"));
    let messages: Vec<String> = [saved.profile_error, record_error, staging_error]
        .into_iter()
        .flatten()
        .collect();
    let save_error = (!messages.is_empty()).then(|| messages.join(". "));
    Ok(FinishOutcome {
        result: FinishResult::Saved {
            zip: saved.zip,
            bytes,
            cpu_profile: saved.cpu_profile,
        },
        save_error,
    })
}

/// Keep a zip that could not be saved to the output folder, and point the
/// session at it. The returned error keeps the original text and names where
/// the zip is.
#[allow(clippy::too_many_arguments)]
async fn keep_zip<F>(
    store: &SessionStore,
    record: &mut SessionRecord,
    zip: Vec<u8>,
    error: CaptureError,
    staged: std::io::Result<()>,
    staging: &Path,
    recovery: &Path,
    dir: &Path,
    places: FinishPlaces<F>,
) -> CaptureError
where
    F: FnOnce() -> std::io::Result<PathBuf> + Send,
{
    let bytes = zip.len();
    let stage_error = match staged {
        Ok(()) => {
            let note = match std::fs::rename(staging, recovery) {
                Ok(()) => {
                    record.zip_path = Some(recovery.to_path_buf());
                    format!("the snapshot zip was kept at {}", recovery.display())
                }
                Err(rename_error) => {
                    record.zip_path = Some(staging.to_path_buf());
                    format!(
                        "the snapshot zip was kept at {} (renaming it to {} failed: {rename_error})",
                        staging.display(),
                        recovery.display()
                    )
                }
            };
            return with_note(error, &save_with_note(store, record, note));
        }
        Err(stage_error) => stage_error,
    };
    // The staging file holds a partial zip.
    let _ = remove_staging(staging);
    let name = format!("{}.zip", record.debugging_context.to_ascii_lowercase());
    let (shown, fallback_error) = match (places.fallback_dir)() {
        Err(dir_error) => ("none".to_string(), dir_error),
        Ok(fallback) => {
            let path = fallback.join(&name);
            match al_bc::output_file::write_no_follow(&path, zip).await {
                Ok(()) => {
                    record.zip_path = Some(path.clone());
                    let note = format!(
                        "the session store could not be written ({stage_error}), so the snapshot zip was kept at {}",
                        path.display()
                    );
                    return with_note(error, &save_with_note(store, record, note));
                }
                Err(write_error) => (path.display().to_string(), write_error),
            }
        }
    };
    internal(format!(
        "The server has finished the capture, and the snapshot zip ({bytes} bytes) could not be written to the session store ({}: {stage_error}), the output folder ({}: {error}) or the recovery folder ({shown}: {fallback_error})",
        staging.display(),
        dir.display(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::any;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A mock BC server that answers every request with `body`.
    async fn serve(body: Vec<u8>) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;
        server
    }

    /// An OnPrem configuration pointing at `mock`, with a dummy tenant.
    fn config_for(mock: &MockServer) -> BcDebugConfig {
        let url = url::Url::parse(&mock.uri()).unwrap();
        BcDebugConfig {
            environment_type: "OnPrem".to_string(),
            server: Some(format!("{}://{}", url.scheme(), url.host_str().unwrap())),
            server_instance: Some("BC".to_string()),
            snapshot_port: url.port(),
            tenant: "00000000-0000-0000-0000-000000000000".to_string(),
            ..BcDebugConfig::default()
        }
    }

    fn point(kind: i32, id: i32, line0: u16, condition: Option<&str>) -> SnappointRecord {
        SnappointRecord {
            object_type: kind,
            object_id: id,
            line0,
            condition: condition.map(str::to_string),
        }
    }

    #[test]
    fn grouping_keeps_first_seen_order_and_dedupes() {
        let groups = group_snappoints(&[
            point(8, 42, 10, None),
            point(5, 80, 5, None),
            point(8, 42, 3, None),
            point(8, 42, 10, None),
            point(8, 42, 10, Some("x")),
            point(5, 80, 5, None),
            point(1, 42, 1, None),
        ])
        .unwrap();
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].object_type, SnapshotObjectType::Page);
        assert_eq!(groups[0].object_number, 42);
        let lines: Vec<_> = groups[0]
            .lines
            .iter()
            .map(|l| (l.line, l.condition.as_str()))
            .collect();
        assert_eq!(lines, vec![(10, ""), (3, ""), (10, "x")]);
        assert_eq!(groups[1].lines.len(), 1);
        assert_eq!(groups[2].object_type, SnapshotObjectType::Table);
    }

    #[test]
    fn grouping_refuses_an_object_type_without_code_lines() {
        let error = group_snappoints(&[point(7, 1, 1, None)]).unwrap_err();
        assert!(matches!(error, CaptureError::Invalid(_)), "{error}");
    }

    #[test]
    fn version_warning_compares_environment_to_sources() {
        assert_eq!(
            version_warning("27.1.0.0", Some("27.1.0.0"), Some("27.1")),
            None
        );
        assert_eq!(version_warning("27.1.0.0", None, None), None);
        let w = version_warning("27.1.0.0", Some("26.5.0.0"), Some("27.1.0.0")).unwrap();
        assert!(w.contains("27.1.0.0") && w.contains("26.5.0.0"), "{w}");
        assert!(!w.contains(".alpackages"), "{w}");
        let w = version_warning("27.1.0.0", None, Some("25.0.0.0")).unwrap();
        assert!(w.contains(".alpackages BaseApp is 25.0.0.0"), "{w}");
    }

    #[test]
    fn started_warning_names_the_seconds_left() {
        let text = started_warning(500);
        assert!(
            text.contains("500 seconds ago") && text.contains("100 seconds left"),
            "{text}"
        );
    }

    #[test]
    fn default_output_dir_is_inside_the_project() {
        let project = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        assert_eq!(
            resolve_output_dir(&root, None).unwrap(),
            root.join(".snapshots")
        );
        assert_eq!(
            resolve_output_dir(&root, Some(Path::new("out/zips"))).unwrap(),
            root.join("out/zips")
        );
    }

    #[test]
    fn output_dir_outside_the_project_is_refused() {
        let project = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        assert!(resolve_output_dir(project.path(), Some(other.path())).is_err());
        assert!(resolve_output_dir(project.path(), Some(Path::new("../x"))).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn default_output_dir_that_is_a_symlink_out_of_the_project_is_refused() {
        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), project.path().join(".snapshots")).unwrap();
        let error = resolve_output_dir(project.path(), None).unwrap_err();
        assert!(matches!(error, CaptureError::Invalid(_)), "{error}");
    }

    #[tokio::test]
    async fn created_output_dir_gets_a_gitignore_and_an_existing_one_does_not() {
        let project = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        let dir = resolve_output_dir(&root, None).unwrap();
        prepare_output_dir(&dir, &root).await.unwrap();
        assert_eq!(std::fs::read(dir.join(".gitignore")).unwrap(), b"*\n");

        let existing = root.join("mine");
        std::fs::create_dir(&existing).unwrap();
        prepare_output_dir(&existing, &root).await.unwrap();
        assert!(!existing.join(".gitignore").exists());
    }

    #[cfg(unix)]
    #[test]
    fn dangling_symlink_output_dir_is_refused() {
        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        std::os::unix::fs::symlink(outside.path().join("missing"), root.join(".snapshots"))
            .unwrap();
        let error = resolve_output_dir(&root, None).unwrap_err();
        assert!(matches!(error, CaptureError::Invalid(_)), "{error}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn existing_symlink_output_dir_is_refused_by_prepare() {
        let project = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("real")).unwrap();
        let link = root.join(".snapshots");
        std::os::unix::fs::symlink(root.join("real"), &link).unwrap();
        let error = prepare_output_dir(&link, &root).await.unwrap_err();
        assert!(matches!(error, CaptureError::Invalid(_)), "{error}");
    }

    #[tokio::test]
    async fn failed_save_keeps_the_zip_in_the_store() {
        let served = EMPTY_ZIP.to_vec();
        let server = serve(served.clone()).await;
        let project = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        // A regular file at the output directory makes the zip save fail.
        std::fs::write(root.join("out.txt"), b"not a directory").unwrap();
        let store_dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(store_dir.path());
        let ctx = "c4fbb573-e325-443c-9887-8d0574e833ee";
        let mut record = SessionRecord {
            debugging_context: ctx.to_string(),
            affinity_cookie: None,
            attach_kind: "UserSession".to_string(),
            created_at: 0,
            project_root: root.clone(),
            configuration_name: None,
            tenant: "00000000-0000-0000-0000-000000000000".to_string(),
            environment_type: "OnPrem".to_string(),
            environment_name: None,
            server: None,
            server_instance: None,
            snapshot_port: None,
            client_type: "WebClient".to_string(),
            user_id: None,
            session_id: -1,
            verbosity: "SnapPoint".to_string(),
            snappoints: Vec::new(),
            last_status: None,
            last_status_at: None,
            started_at: None,
            zip_path: None,
            finished_at: None,
            profile: None,
        };
        store.save(&record).unwrap();
        let error = finish(
            &reqwest::Client::new(),
            "token",
            &store,
            &config_for(&server),
            false,
            &mut record,
            Some(Path::new("out.txt")),
        )
        .await
        .err()
        .expect("finish fails when the zip cannot be saved");
        let recovery = store.recovery_path(ctx).unwrap();
        let text = error.to_string();
        assert!(text.contains(&recovery.display().to_string()), "{text}");
        assert_eq!(std::fs::read(&recovery).unwrap(), served);
        assert_eq!(store.load(ctx).unwrap().unwrap().zip_path, Some(recovery));
    }

    /// A minimal empty zip: the 22 byte end of central directory record.
    const EMPTY_ZIP: &[u8] =
        b"PK\x05\x06\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00";

    const CTX: &str = "c4fbb573-e325-443c-9887-8d0574e833ee";

    fn record_in(root: &Path) -> SessionRecord {
        SessionRecord {
            debugging_context: CTX.to_string(),
            affinity_cookie: None,
            attach_kind: "UserSession".to_string(),
            created_at: 0,
            project_root: root.to_path_buf(),
            configuration_name: None,
            tenant: "00000000-0000-0000-0000-000000000000".to_string(),
            environment_type: "OnPrem".to_string(),
            environment_name: None,
            server: None,
            server_instance: None,
            snapshot_port: None,
            client_type: "WebClient".to_string(),
            user_id: None,
            session_id: -1,
            verbosity: "SnapPoint".to_string(),
            snappoints: Vec::new(),
            last_status: None,
            last_status_at: None,
            started_at: None,
            zip_path: None,
            finished_at: None,
            profile: None,
        }
    }

    fn staging_in(store: &Path) -> PathBuf {
        store.join(format!(".{CTX}.zip.partial"))
    }

    fn failing_write(_: &mut std::fs::File, _: &[u8]) -> std::io::Result<()> {
        Err(std::io::Error::other("disk full"))
    }

    /// A server whose finish must not be called.
    async fn serve_nothing() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"zip".to_vec()))
            .expect(0)
            .mount(&server)
            .await;
        server
    }

    async fn run_finish(
        server: &MockServer,
        store: &SessionStore,
        record: &mut SessionRecord,
        output: Option<&Path>,
    ) -> Result<FinishOutcome> {
        finish(
            &reqwest::Client::new(),
            "token",
            store,
            &config_for(server),
            false,
            record,
            output,
        )
        .await
    }

    #[tokio::test]
    async fn unwritable_store_refuses_finish_before_the_server_call() {
        let server = serve_nothing().await;
        let project = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        let other = tempfile::tempdir().unwrap();
        let store_path = other.path().join("store");
        std::fs::write(&store_path, b"a file, not a folder").unwrap();
        let store = SessionStore::at(&store_path);
        let mut record = record_in(&root);
        let error = run_finish(&server, &store, &mut record, None)
            .await
            .err()
            .expect("an unwritable store refuses finish");
        assert!(matches!(error, CaptureError::Invalid(_)), "{error}");
        let text = error.to_string();
        assert!(text.contains(&store_path.display().to_string()), "{text}");
        assert!(record.finished_at.is_none());
        server.verify().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stale_staging_link_is_refused() {
        let server = serve_nothing().await;
        let project = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        let store_dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(store_dir.path());
        std::os::unix::fs::symlink(root.join("elsewhere"), staging_in(store_dir.path())).unwrap();
        let mut record = record_in(&root);
        let error = run_finish(&server, &store, &mut record, None)
            .await
            .err()
            .expect("a link at the staging name is refused");
        assert!(matches!(error, CaptureError::Invalid(_)), "{error}");
        assert!(!root.join("elsewhere").exists());
        server.verify().await;
    }

    #[tokio::test]
    async fn staging_file_is_removed_after_a_good_save() {
        let served = EMPTY_ZIP.to_vec();
        let server = serve(served.clone()).await;
        let project = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        let store_dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(store_dir.path());
        let mut record = record_in(&root);
        let outcome = run_finish(&server, &store, &mut record, None)
            .await
            .unwrap();
        let FinishResult::Saved { zip, .. } = outcome.result else {
            panic!("expected a saved zip");
        };
        assert_eq!(std::fs::read(zip).unwrap(), served);
        assert!(!staging_in(store_dir.path()).exists());
        assert!(outcome.save_error.is_none(), "{:?}", outcome.save_error);
    }

    #[tokio::test]
    async fn stale_staging_file_does_not_block_finish() {
        let server = serve(EMPTY_ZIP.to_vec()).await;
        let project = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        let store_dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(store_dir.path());
        std::fs::write(staging_in(store_dir.path()), b"left by a crashed run").unwrap();
        let mut record = record_in(&root);
        run_finish(&server, &store, &mut record, None)
            .await
            .unwrap();
        assert!(!staging_in(store_dir.path()).exists());
    }

    #[tokio::test]
    async fn failed_server_finish_leaves_no_staging_file_and_retry_works() {
        let served = EMPTY_ZIP.to_vec();
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200).set_body_bytes(served.clone()))
            .with_priority(2)
            .mount(&server)
            .await;
        let project = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        let store_dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(store_dir.path());
        let mut record = record_in(&root);
        assert!(run_finish(&server, &store, &mut record, None)
            .await
            .is_err());
        assert!(!staging_in(store_dir.path()).exists());
        assert!(record.finished_at.is_none());
        let outcome = run_finish(&server, &store, &mut record, None)
            .await
            .unwrap();
        let FinishResult::Saved { zip, .. } = outcome.result else {
            panic!("expected a saved zip");
        };
        assert_eq!(std::fs::read(zip).unwrap(), served);
    }

    #[tokio::test]
    async fn full_store_and_output_fall_back_to_the_runtime_folder() {
        let served = EMPTY_ZIP.to_vec();
        let server = serve(served.clone()).await;
        let project = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        std::fs::write(root.join("out.txt"), b"not a directory").unwrap();
        let store_dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(store_dir.path());
        let fallback = tempfile::tempdir().unwrap();
        let fallback_path = fallback.path().to_path_buf();
        let mut record = record_in(&root);
        store.save(&record).unwrap();
        let error = finish_with(
            &reqwest::Client::new(),
            "token",
            &store,
            &config_for(&server),
            false,
            &mut record,
            Some(Path::new("out.txt")),
            FinishPlaces {
                fallback_dir: move || Ok(fallback_path),
                write_staging: failing_write,
            },
        )
        .await
        .err()
        .expect("finish fails when the output save fails");
        let kept = fallback.path().join(format!("{CTX}.zip"));
        let text = error.to_string();
        assert!(text.contains(&kept.display().to_string()), "{text}");
        assert_eq!(std::fs::read(&kept).unwrap(), served);
        assert!(!staging_in(store_dir.path()).exists());
        assert_eq!(store.load(CTX).unwrap().unwrap().zip_path, Some(kept));
    }

    #[tokio::test]
    async fn all_destinations_failing_names_them_and_the_byte_count() {
        let served = EMPTY_ZIP.to_vec();
        let server = serve(served.clone()).await;
        let project = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        std::fs::write(root.join("out.txt"), b"not a directory").unwrap();
        let store_dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(store_dir.path());
        let mut record = record_in(&root);
        let error = finish_with(
            &reqwest::Client::new(),
            "token",
            &store,
            &config_for(&server),
            false,
            &mut record,
            Some(Path::new("out.txt")),
            FinishPlaces {
                fallback_dir: || Err(std::io::Error::other("no runtime folder")),
                write_staging: failing_write,
            },
        )
        .await
        .err()
        .expect("finish fails when nothing can be written");
        let text = error.to_string();
        assert!(text.contains("has finished the capture"), "{text}");
        assert!(
            text.contains(&format!("({} bytes)", served.len())),
            "{text}"
        );
        let staging = staging_in(store_dir.path());
        assert!(text.contains(&staging.display().to_string()), "{text}");
        assert!(
            text.contains(&root.join("out.txt").display().to_string()),
            "{text}"
        );
        assert!(text.contains("recovery folder"), "{text}");
        assert!(!staging.exists());
    }

    #[tokio::test]
    async fn status_keeps_a_finish_saved_by_another_process() {
        let server = serve(b"Finished".to_vec()).await;
        let project = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        let store_dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(store_dir.path());
        let mut stored = record_in(&root);
        stored.finished_at = Some(5);
        stored.zip_path = Some(root.join(".snapshots").join(format!("{CTX}.zip")));
        store.save(&stored).unwrap();
        let mut stale = record_in(&root);
        status(
            &reqwest::Client::new(),
            "token",
            &store,
            &config_for(&server),
            false,
            &mut stale,
        )
        .await
        .unwrap();
        let reloaded = store.load(CTX).unwrap().unwrap();
        assert_eq!(reloaded.finished_at, Some(5));
        assert_eq!(reloaded.zip_path, stored.zip_path);
    }
}
