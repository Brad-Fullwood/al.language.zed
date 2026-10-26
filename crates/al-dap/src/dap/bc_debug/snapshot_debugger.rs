//! Snapshot debugger REST calls: endpoint metadata, attach, status and finish.
//!
//! Routes checked before writing this:
//! - Microsoft's platform snapshot MCP proxy (`al launchsnapshotmcpproxy`,
//!   streamable HTTP to `<base>/mcp/`). It signs in with the AL tool's own
//!   Entra login and takes no token. On a BC 29.0 Sandbox (snapshot web API
//!   5.0) the upstream `initialize` answers, but `tools/list` for the
//!   debugging tool sets fails with -32603 until the environment's MCP Server
//!   Configuration allows them. There is no read only way to check that
//!   setting, so it is not relied on.
//! - The AL host's LSP route (`al/initializeSnapshotDebuggerAttach` and the
//!   calls after it). It works but needs .NET and the AL extension installed and
//!   a host process for each call.
//! - Native REST, chosen. It sends the same four requests as the host's
//!   `SnapshotDebuggerClient` (recorded byte for byte, see
//!   `tests/fixtures/snapshot_debugger`) and reuses the debugger's
//!   [`BcDebugConfig`], `reqwest::Client` and token path, so there is no new
//!   client or sign in.
//!
//! The functions take the access token as a parameter like `rest.rs` does. This
//! module is unrelated to the `al-snapshot` crate and the test snapshot feature.

use std::fmt;
use std::sync::OnceLock;

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, COOKIE, ETAG, SET_COOKIE};
use serde::Serialize;
use uuid::Uuid;

use super::session_config::BcDebugConfig;
use super::wire::percent_encode_url;

const AFFINITY_COOKIE: &str = "ApplicationGatewayAffinity";
const ACTIVITY_ID_HEADER: &str = "d365-bc-client-activity-telemetry-id";
const SESSION_ID_HEADER: &str = "d365-bc-client-session-telemetry-id";
const CORRELATION_ID_HEADER: &str = "x-ms-correlation-id";

/// Sessions the server drops: unattached for 30 minutes, or 10 minutes after start.
const NOT_FOUND_MESSAGE: &str = "the server no longer knows this debugging context: the session ended, timed out (30 minutes unattached, 10 minutes after start) or was dropped";

#[derive(Debug, thiserror::Error)]
pub enum SnapshotDebuggerError {
    #[error("refusing to use the snapshot debugger on a Production environment without allow_production")]
    Production,
    #[error("snapshot debugger request failed (request {correlation_id}): {message}")]
    Request {
        correlation_id: String,
        message: String,
    },
    #[error(
        "snapshot debugger {operation} returned HTTP {status} (request {correlation_id}): {body}"
    )]
    Http {
        operation: &'static str,
        status: u16,
        correlation_id: String,
        body: String,
    },
    #[error(
        "snapshot debugger {operation} failed (request {correlation_id}): {NOT_FOUND_MESSAGE}"
    )]
    NotFound {
        operation: &'static str,
        correlation_id: String,
    },
    #[error("snapshot debugger {operation} sent a reply that could not be read (request {correlation_id}): {message}")]
    BadResponse {
        operation: &'static str,
        correlation_id: String,
        message: String,
    },
}

impl SnapshotDebuggerError {
    /// The `x-ms-correlation-id` of the request that failed, for logs. Empty
    /// when no request was sent (the Production guard).
    pub fn correlation_id(&self) -> &str {
        match self {
            Self::Production => "",
            Self::Request { correlation_id, .. }
            | Self::Http { correlation_id, .. }
            | Self::NotFound { correlation_id, .. }
            | Self::BadResponse { correlation_id, .. } => correlation_id,
        }
    }
}

pub type Result<T> = std::result::Result<T, SnapshotDebuggerError>;

/// A new debugging context GUID for an attach.
pub fn new_debugging_context() -> String {
    Uuid::new_v4().to_string()
}

/// `ClientType` of the session to snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotClientType {
    WebServiceClient = 0,
    WebClient = 1,
    Background = 2,
    ClientService = 3,
}

/// `SnapshotVerbosity`: snap points only, or full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotVerbosity {
    SnapPoint = 0,
    Full = 1,
}

/// `ObjectTypeWrapper` values the host sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotObjectType {
    Table = 1,
    Report = 3,
    CodeUnit = 5,
    XmlPort = 6,
    Page = 8,
    Query = 9,
    PageExtension = 14,
    TableExtension = 15,
    Enum = 16,
    EnumExtension = 17,
    ReportExtension = 22,
    Interface = 28,
}

/// `ExecutionContext` flags: 1 debugging, 2 profiling. The AL extension defaults
/// to both (DebugAndProfile).
pub const EXECUTION_CONTEXT_DEBUG_AND_PROFILE: u32 = 3;

/// One snap point: a 0 based line in an object and an optional condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotLine {
    pub line: u16,
    pub condition: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotBreakpointLocation {
    pub object_type: SnapshotObjectType,
    pub object_number: u32,
    pub lines: Vec<SnapshotLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotAttachRequest {
    pub debugging_context: String,
    pub client_type: SnapshotClientType,
    pub user_id: Option<String>,
    pub locations: Vec<SnapshotBreakpointLocation>,
    pub verbosity: SnapshotVerbosity,
    /// `-1` means no specific session.
    pub session_id: i64,
    pub execution_context: u32,
}

impl SnapshotAttachRequest {
    pub fn new(debugging_context: String) -> Self {
        Self {
            debugging_context,
            client_type: SnapshotClientType::WebClient,
            user_id: None,
            locations: Vec::new(),
            verbosity: SnapshotVerbosity::Full,
            session_id: -1,
            execution_context: EXECUTION_CONTEXT_DEBUG_AND_PROFILE,
        }
    }
}

// Wire shapes. Field order is the order Microsoft's serializer writes, and
// zero values are left out like its DefaultValueHandling.Ignore does.
fn is_zero<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct AttachBody<'a> {
    debugging_context: &'a str,
    #[serde(skip_serializing_if = "is_zero")]
    client_type: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_id: Option<&'a str>,
    source_breakpoint_locations: Vec<LocationBody<'a>>,
    #[serde(skip_serializing_if = "is_zero")]
    snapshot_verbosity: u8,
    #[serde(skip_serializing_if = "is_zero")]
    session_id: i64,
    #[serde(skip_serializing_if = "is_zero")]
    execution_context: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct LocationBody<'a> {
    id: IdBody,
    lines: Vec<LineBody<'a>>,
}

#[derive(Serialize)]
struct IdBody {
    #[serde(rename = "objectNumber")]
    object_number_lower: u32,
    #[serde(rename = "objectType")]
    object_type_lower: u32,
    #[serde(rename = "ObjectNumber")]
    object_number: u32,
    #[serde(rename = "ObjectType")]
    object_type: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct LineBody<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    position: Option<PositionBody>,
    condition: &'a str,
}

/// `Column` is always 0 so it is never written.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct PositionBody {
    line: u16,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct ContextBody<'a> {
    debugging_context: &'a str,
}

/// The request body `attach` sends.
pub fn attach_body(request: &SnapshotAttachRequest) -> String {
    let locations = request
        .locations
        .iter()
        .map(|location| LocationBody {
            id: IdBody {
                object_number_lower: location.object_number,
                object_type_lower: location.object_type as u32,
                object_number: location.object_number,
                object_type: location.object_type as u32,
            },
            lines: location
                .lines
                .iter()
                .map(|line| LineBody {
                    position: (line.line != 0).then_some(PositionBody { line: line.line }),
                    condition: &line.condition,
                })
                .collect(),
        })
        .collect();
    let body = AttachBody {
        debugging_context: &request.debugging_context,
        client_type: request.client_type as u8,
        user_id: request.user_id.as_deref(),
        source_breakpoint_locations: locations,
        snapshot_verbosity: request.verbosity as u8,
        session_id: request.session_id,
        execution_context: request.execution_context,
    };
    serde_json::to_string(&body).expect("attach body serialises")
}

fn context_body(debugging_context: &str) -> String {
    serde_json::to_string(&ContextBody { debugging_context }).expect("context body serialises")
}

/// What `snapshotendpointmetadata` returns. Fields the server adds are ignored.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct SnapshotEndpointMetadata {
    #[serde(rename = "runtimeVersion", alias = "RuntimeVersion", default)]
    pub runtime_version: Option<String>,
    #[serde(rename = "webApiVersion", alias = "WebApiVersion", default)]
    pub web_api_version: Option<String>,
    #[serde(rename = "webEndpoint", alias = "WebEndpoint", default)]
    pub web_endpoint: Option<String>,
}

/// The attach kind the server answers with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotAttachKind {
    Undefined,
    UserSession,
    NextSessionForUserOnTenant,
    NextSessionOnTenant,
}

impl SnapshotAttachKind {
    fn parse(text: &str) -> Option<Self> {
        Some(match text.trim() {
            "Undefined" => Self::Undefined,
            "UserSession" => Self::UserSession,
            "NextSessionForUserOnTenant" => Self::NextSessionForUserOnTenant,
            "NextSessionOnTenant" => Self::NextSessionOnTenant,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotAttachResult {
    pub kind: SnapshotAttachKind,
    /// `ApplicationGatewayAffinity` value exactly as the server sent it, for
    /// example `abc%3D%3D`. Pass it to status and finish.
    pub affinity_cookie: Option<String>,
    pub correlation_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotStatus {
    Failed,
    Initialized,
    Started,
    Finished,
}

impl fmt::Display for SnapshotStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Failed => "Failed",
            Self::Initialized => "Initialized",
            Self::Started => "Started",
            Self::Finished => "Finished",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotStatusResult {
    pub status: SnapshotStatus,
    pub correlation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotFinishResult {
    /// The snapshot zip. `None` when the body was empty: nothing was recorded.
    pub zip: Option<Vec<u8>>,
    /// ETag was `"Sampling"`: the zip holds `<ctx>.alcpuprofile`.
    pub sampling: bool,
    pub correlation_id: String,
}

fn session_telemetry_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| Uuid::new_v4().to_string())
}

/// Refuse Production unless the caller says it is allowed.
pub fn guard_production(config: &BcDebugConfig, allow_production: bool) -> Result<()> {
    let production = config.environment_type.eq_ignore_ascii_case("Production")
        || config
            .environment_name
            .as_deref()
            .is_some_and(|name| name.eq_ignore_ascii_case("Production"));
    if production && !allow_production {
        return Err(SnapshotDebuggerError::Production);
    }
    Ok(())
}

/// The tenant is escaped once here and once more in the query string.
fn tenant_param(config: &BcDebugConfig) -> String {
    percent_encode_url(&percent_encode_url(&config.tenant))
}

struct Sent {
    response: reqwest::Response,
    correlation_id: String,
}

/// Send one request with Microsoft's header order: Authorization, activity id,
/// session id, correlation id, Cookie, Content-Type. `reqwest` also adds
/// `Accept: */*` and `Host`, which the host does not send.
#[allow(clippy::too_many_arguments)]
async fn send(
    http: &reqwest::Client,
    config: &BcDebugConfig,
    access_token: &str,
    allow_production: bool,
    operation: &'static str,
    query: &str,
    cookie: Option<&str>,
    body: Option<String>,
) -> Result<Sent> {
    guard_production(config, allow_production)?;
    let activity = Uuid::new_v4().to_string();
    let url = format!("{}/{operation}?{query}", config.snapshot_base_url());
    let mut request = match body {
        Some(_) => http.post(&url),
        None => http.get(&url),
    };
    request = request
        .header(AUTHORIZATION, format!("Bearer {access_token}"))
        .header(ACTIVITY_ID_HEADER, &activity)
        .header(SESSION_ID_HEADER, session_telemetry_id())
        .header(CORRELATION_ID_HEADER, &activity);
    if let Some(cookie) = cookie {
        request = request.header(COOKIE, format!("{AFFINITY_COOKIE}={cookie}"));
    }
    if let Some(body) = body {
        request = request
            .header(CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(body);
    }
    let response = request
        .send()
        .await
        .map_err(|e| SnapshotDebuggerError::Request {
            correlation_id: activity.clone(),
            message: e.without_url().to_string(),
        })?;
    Ok(Sent {
        response,
        correlation_id: activity,
    })
}

/// Turn a non 2xx reply into an error. 404 gets its own variant.
async fn check_status(sent: Sent, operation: &'static str) -> Result<Sent> {
    let status = sent.response.status();
    if status.is_success() {
        return Ok(sent);
    }
    let correlation_id = sent.correlation_id;
    if status.as_u16() == 404 && operation != "attach" && operation != "snapshotendpointmetadata" {
        return Err(SnapshotDebuggerError::NotFound {
            operation,
            correlation_id,
        });
    }
    let body = al_bc::bc_client::read_error_body_capped(sent.response).await;
    Err(SnapshotDebuggerError::Http {
        operation,
        status: status.as_u16(),
        correlation_id,
        body,
    })
}

async fn text_body(sent: Sent, operation: &'static str) -> Result<(String, String)> {
    let correlation_id = sent.correlation_id;
    let bytes = al_bc::bc_client::read_binary_body_capped(sent.response)
        .await
        .map_err(|e| SnapshotDebuggerError::BadResponse {
            operation,
            correlation_id: correlation_id.clone(),
            message: al_bc::bc_client::sanitize_error_body(&e.to_string()),
        })?;
    Ok((String::from_utf8_lossy(&bytes).into_owned(), correlation_id))
}

fn bad_response(
    operation: &'static str,
    correlation_id: &str,
    message: String,
) -> SnapshotDebuggerError {
    SnapshotDebuggerError::BadResponse {
        operation,
        correlation_id: correlation_id.to_string(),
        message: al_bc::bc_client::sanitize_error_body(&message),
    }
}

/// `GET snapshotendpointmetadata?tenant=...`
pub async fn snapshot_endpoint_metadata(
    http: &reqwest::Client,
    config: &BcDebugConfig,
    access_token: &str,
    allow_production: bool,
) -> Result<(SnapshotEndpointMetadata, String)> {
    const OP: &str = "snapshotendpointmetadata";
    let query = format!("tenant={}", tenant_param(config));
    let sent = send(
        http,
        config,
        access_token,
        allow_production,
        OP,
        &query,
        None,
        None,
    )
    .await?;
    let (text, correlation_id) = text_body(check_status(sent, OP).await?, OP).await?;
    let metadata = serde_json::from_str(&text)
        .map_err(|e| bad_response(OP, &correlation_id, e.to_string()))?;
    Ok((metadata, correlation_id))
}

/// `POST attach?debuggingcontext=..[&sessionid=..][&userid=..]&tenant=..`
pub async fn snapshot_attach(
    http: &reqwest::Client,
    config: &BcDebugConfig,
    access_token: &str,
    allow_production: bool,
    request: &SnapshotAttachRequest,
) -> Result<SnapshotAttachResult> {
    const OP: &str = "attach";
    let mut query = format!(
        "debuggingcontext={}",
        percent_encode_url(&request.debugging_context)
    );
    if request.session_id != -1 {
        query.push_str(&format!("&sessionid={}", request.session_id));
    }
    if let Some(user_id) = &request.user_id {
        query.push_str(&format!("&userid={}", percent_encode_url(user_id)));
    }
    query.push_str(&format!("&tenant={}", tenant_param(config)));
    let sent = send(
        http,
        config,
        access_token,
        allow_production,
        OP,
        &query,
        None,
        Some(attach_body(request)),
    )
    .await?;
    let sent = check_status(sent, OP).await?;
    let affinity_cookie = sent
        .response
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            let value = value.strip_prefix(AFFINITY_COOKIE)?.strip_prefix('=')?;
            Some(value.split(';').next().unwrap_or(value).to_string())
        });
    let (text, correlation_id) = text_body(sent, OP).await?;
    let kind = SnapshotAttachKind::parse(&text).ok_or_else(|| {
        bad_response(OP, &correlation_id, format!("unknown attach kind `{text}`"))
    })?;
    Ok(SnapshotAttachResult {
        kind,
        affinity_cookie,
        correlation_id,
    })
}

fn context_query(debugging_context: &str, cookie: Option<&str>, config: &BcDebugConfig) -> String {
    let mut query = format!("debuggingcontext={}", percent_encode_url(debugging_context));
    if let Some(cookie) = cookie {
        query.push_str(&format!(
            "&applicationgatewayaffinity={}",
            percent_encode_url(cookie)
        ));
    }
    query.push_str(&format!("&tenant={}", tenant_param(config)));
    query
}

/// `POST status`
pub async fn snapshot_status(
    http: &reqwest::Client,
    config: &BcDebugConfig,
    access_token: &str,
    allow_production: bool,
    debugging_context: &str,
    affinity_cookie: Option<&str>,
) -> Result<SnapshotStatusResult> {
    const OP: &str = "status";
    let sent = send(
        http,
        config,
        access_token,
        allow_production,
        OP,
        &context_query(debugging_context, affinity_cookie, config),
        affinity_cookie,
        Some(context_body(debugging_context)),
    )
    .await?;
    let (text, correlation_id) = text_body(check_status(sent, OP).await?, OP).await?;
    let status = match text.trim() {
        "Failed" => SnapshotStatus::Failed,
        "Initialized" => SnapshotStatus::Initialized,
        "Started" => SnapshotStatus::Started,
        "Finished" => SnapshotStatus::Finished,
        other => {
            return Err(bad_response(
                OP,
                &correlation_id,
                format!("unknown status `{other}`"),
            ));
        }
    };
    Ok(SnapshotStatusResult {
        status,
        correlation_id,
    })
}

/// `POST finish`. Save the zip with `al_bc::snapshot::save_finished_snapshot`.
pub async fn snapshot_finish(
    http: &reqwest::Client,
    config: &BcDebugConfig,
    access_token: &str,
    allow_production: bool,
    debugging_context: &str,
    affinity_cookie: Option<&str>,
) -> Result<SnapshotFinishResult> {
    const OP: &str = "finish";
    let sent = send(
        http,
        config,
        access_token,
        allow_production,
        OP,
        &context_query(debugging_context, affinity_cookie, config),
        affinity_cookie,
        Some(context_body(debugging_context)),
    )
    .await?;
    let sent = check_status(sent, OP).await?;
    let sampling = sent
        .response
        .headers()
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim_matches('"') == "Sampling");
    let correlation_id = sent.correlation_id.clone();
    let bytes = al_bc::bc_client::read_binary_body_capped(sent.response)
        .await
        .map_err(|e| bad_response(OP, &correlation_id, e.to_string()))?;
    Ok(SnapshotFinishResult {
        zip: (!bytes.is_empty()).then_some(bytes),
        sampling,
        correlation_id,
    })
}

#[cfg(test)]
#[path = "snapshot_debugger_tests.rs"]
mod tests;
