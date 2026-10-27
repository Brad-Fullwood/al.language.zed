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
    new_debugging_context, snapshot_attach, snapshot_finish, snapshot_status, SnapshotAttachKind,
    SnapshotAttachRequest, SnapshotBreakpointLocation, SnapshotClientType, SnapshotDebuggerError,
    SnapshotLine, SnapshotObjectType, SnapshotVerbosity,
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

/// Attach the snappoints and remember the session.
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

    let debugging_context = new_debugging_context();
    let mut attach = SnapshotAttachRequest::new(debugging_context.clone());
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

/// Poll the server, note the answer in `record` and save it.
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
    let mut tail = PathBuf::new();
    let canonical = loop {
        match existing.canonicalize() {
            Ok(path) => break path,
            Err(_) => {
                let name = existing.file_name().ok_or_else(escapes)?;
                let mut next = PathBuf::from(name);
                next.push(&tail);
                tail = next;
                existing = existing.parent().ok_or_else(escapes)?;
            }
        }
    };
    let resolved = canonical.join(tail);
    if !resolved.starts_with(&root) {
        return Err(escapes());
    }
    Ok(resolved)
}

/// Create `dir` when it is missing and, only then, write a `.gitignore`
/// holding `*` into it, because snapshot zips hold customer data. An existing
/// directory is left as it is. The created directory is checked against the
/// project root again, which catches a link planted between the check and the
/// creation.
async fn prepare_output_dir(dir: &Path, project_root: &Path) -> Result<()> {
    if tokio::fs::symlink_metadata(dir).await.is_ok() {
        return Ok(());
    }
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|error| internal(format!("cannot create {}: {error}", dir.display())))?;
    let created = resolve_output_dir(project_root, Some(dir))?;
    if created != dir {
        return Err(invalid(format!(
            "snapshot output directory '{}' resolves to '{}', which is not where it was checked",
            dir.display(),
            created.display()
        )));
    }
    al_bc::output_file::write_no_follow(&dir.join(".gitignore"), b"*\n".to_vec())
        .await
        .map_err(|error| {
            internal(format!(
                "cannot write the .gitignore in {}: {error}",
                dir.display()
            ))
        })?;
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

/// Finish the session, save the zip into `output_dir` (default
/// `<project>/.snapshots`) and update the record. The directory is checked
/// before the server is asked to finish, so a refused directory does not cost
/// the snapshot. It is created only when there is a zip to save.
pub async fn finish(
    http: &reqwest::Client,
    token: &str,
    store: &SessionStore,
    config: &BcDebugConfig,
    allow_production: bool,
    record: &mut SessionRecord,
    output_dir: Option<&Path>,
) -> Result<FinishOutcome> {
    let dir = resolve_output_dir(&record.project_root, output_dir)?;
    let finished = snapshot_finish(
        http,
        config,
        token,
        allow_production,
        &record.debugging_context,
        record.affinity_cookie.as_deref(),
    )
    .await?;
    let now = now_secs();
    record.finished_at = Some(now);
    record.last_status = Some("Finished".to_string());
    record.last_status_at = Some(now);
    let Some(zip) = finished.zip else {
        let save_error = store
            .save(record)
            .err()
            .map(|error| format!("The session could not be updated: {error}"));
        return Ok(FinishOutcome {
            result: FinishResult::Empty,
            save_error,
        });
    };
    let bytes = zip.len();
    prepare_output_dir(&dir, &record.project_root).await?;
    let saved = al_bc::snapshot::save_finished_snapshot(
        &dir,
        &record.debugging_context,
        zip,
        finished.sampling,
    )
    .await
    .map_err(|error| internal(format!("cannot save the snapshot: {error}")))?;
    record.zip_path = Some(saved.zip.clone());
    let save_error = store
        .save(record)
        .err()
        .map(|error| format!("The session could not be updated: {error}"));
    Ok(FinishOutcome {
        result: FinishResult::Saved {
            zip: saved.zip,
            bytes,
            cpu_profile: saved.cpu_profile,
        },
        save_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
