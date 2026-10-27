//! Tests drive each call against a wiremock server that answers with a recording
//! of Microsoft's AL host (`tests/fixtures/snapshot_debugger`) and compare what
//! arrived with what the host sent.

use super::*;
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const TENANT: &str = "11111111-2222-3333-4444-555555555555";
const ODD_TENANT: &str = "my tenant/x+y";
const TOKEN: &str = "RECORDING-FAKE-TOKEN";
const COOKIE: &str = "abcdefghij0123456789AB%3D%3D";
const USER: &str = "00000000-0000-0000-0000-0000000000aa";

struct Recording {
    method: String,
    raw_target: String,
    headers: Vec<(String, String)>,
    body_text: String,
    status: u16,
    response_headers: Vec<(String, String)>,
    response_body: Vec<u8>,
}

fn pairs(value: &serde_json::Value) -> Vec<(String, String)> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p[0].as_str().unwrap().to_string(),
                p[1].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn base64_decode(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in text.bytes().filter(|c| *c != b'=') {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            _ => 63,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    out
}

fn recording(name: &str) -> Recording {
    let path = format!(
        "{}/tests/fixtures/snapshot_debugger/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    Recording {
        method: v["method"].as_str().unwrap().to_string(),
        raw_target: v["raw_target"].as_str().unwrap().to_string(),
        headers: pairs(&v["headers"]),
        body_text: v["body_text"].as_str().unwrap().to_string(),
        status: v["response"]["status"].as_u64().unwrap() as u16,
        response_headers: pairs(&v["response"]["headers"]),
        response_body: base64_decode(v["response"]["body_b64"].as_str().unwrap()),
    }
}

async fn serve(rec: &Recording) -> MockServer {
    let server = MockServer::start().await;
    let mut template = ResponseTemplate::new(rec.status);
    for (name, value) in &rec.response_headers {
        template = template.append_header(name.as_str(), value.as_str());
    }
    template = template.set_body_bytes(rec.response_body.clone());
    Mock::given(any())
        .respond_with(template)
        .mount(&server)
        .await;
    server
}

fn config_for(mock: &MockServer, tenant: &str) -> BcDebugConfig {
    let url = url::Url::parse(&mock.uri()).unwrap();
    BcDebugConfig {
        environment_type: "OnPrem".to_string(),
        server: Some(format!("{}://{}", url.scheme(), url.host_str().unwrap())),
        server_instance: Some("BC".to_string()),
        snapshot_port: url.port(),
        tenant: tenant.to_string(),
        ..BcDebugConfig::default()
    }
}

fn names_without_client_extras(headers: impl Iterator<Item = String>) -> Vec<String> {
    headers
        .map(|n| n.to_ascii_lowercase())
        .filter(|n| !matches!(n.as_str(), "host" | "accept" | "content-length"))
        .collect()
}

/// Assert the single request the mock saw equals the recording, apart from the
/// GUID headers (random) and the `Host` and `Accept` headers `reqwest` adds.
async fn assert_matches(server: &MockServer, rec: &Recording, op: &str) -> Request {
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let got = requests[0].clone();
    assert_eq!(got.method.as_str(), rec.method);
    assert!(got.url.path().ends_with(&format!("/snapshotdebugger/{op}")));
    let (_, want_query) = rec.raw_target.split_once('?').unwrap();
    assert_eq!(got.url.query().unwrap(), want_query, "raw query");
    assert_eq!(String::from_utf8(got.body.clone()).unwrap(), rec.body_text);
    assert_eq!(
        names_without_client_extras(got.headers.keys().map(|k| k.as_str().to_string())),
        names_without_client_extras(rec.headers.iter().map(|(n, _)| n.clone())),
        "header names and order"
    );
    let value = |name: &str| {
        got.headers
            .get(name)
            .map(|v| v.to_str().unwrap().to_string())
    };
    for exact in ["Authorization", "Content-Type", "Cookie"] {
        let want = rec
            .headers
            .iter()
            .find(|(n, _)| n == exact)
            .map(|(_, v)| v.clone());
        assert_eq!(value(exact), want, "{exact}");
    }
    let activity = value(ACTIVITY_ID_HEADER).unwrap();
    assert!(Uuid::parse_str(&activity).is_ok());
    assert!(Uuid::parse_str(&value(SESSION_ID_HEADER).unwrap()).is_ok());
    assert_eq!(value(CORRELATION_ID_HEADER).unwrap(), activity);
    got
}

fn line(line: u16, condition: &str) -> SnapshotLine {
    SnapshotLine {
        line,
        condition: condition.to_string(),
    }
}

fn codeunit_22(lines: Vec<SnapshotLine>) -> SnapshotBreakpointLocation {
    SnapshotBreakpointLocation {
        object_type: SnapshotObjectType::CodeUnit,
        object_number: 22,
        lines,
    }
}

fn request(ctx: &str, lines: Vec<SnapshotLine>) -> SnapshotAttachRequest {
    SnapshotAttachRequest {
        locations: vec![codeunit_22(lines)],
        ..SnapshotAttachRequest::new(ctx.to_string())
    }
}

#[tokio::test]
async fn metadata_matches_recording_and_tolerates_extra_fields() {
    let rec = recording("run1-1-snapshotendpointmetadata");
    let server = serve(&rec).await;
    let http = reqwest::Client::new();
    let (meta, id) = snapshot_endpoint_metadata(&http, &config_for(&server, TENANT), TOKEN, false)
        .await
        .unwrap();
    assert_matches(&server, &rec, "snapshotendpointmetadata").await;
    assert_eq!(meta.web_api_version.as_deref(), Some("5.0.0.0"));
    assert_eq!(meta.runtime_version.as_deref(), Some("15.0.0.0"));
    assert!(Uuid::parse_str(&id).is_ok());
}

#[test]
fn metadata_parses_the_sandbox_sample() {
    let meta: SnapshotEndpointMetadata = serde_json::from_str(
        r#"{"runtimeVersion":"17.1","webApiVersion":"3.0","webEndpoint":"https://businesscentral.dynamics.com","other":1}"#,
    )
    .unwrap();
    assert_eq!(meta.web_api_version.as_deref(), Some("3.0"));
    assert_eq!(
        meta.web_endpoint.as_deref(),
        Some("https://businesscentral.dynamics.com")
    );
}

async fn run_attach(
    name: &str,
    tenant: &str,
    req: SnapshotAttachRequest,
) -> (SnapshotAttachResult, Request) {
    let rec = recording(name);
    let server = serve(&rec).await;
    let http = reqwest::Client::new();
    let result = snapshot_attach(&http, &config_for(&server, tenant), TOKEN, false, &req)
        .await
        .unwrap();
    let got = assert_matches(&server, &rec, "attach").await;
    (result, got)
}

#[tokio::test]
async fn attach_run1_web_client_user_conditions_and_two_objects() {
    let mut req = request(
        "ed75bfc5-a198-4fe6-ad81-54b95d34965a",
        vec![line(174, ""), line(426, "Rec.\"No.\" = 'X'")],
    );
    req.user_id = Some(USER.to_string());
    req.locations.push(SnapshotBreakpointLocation {
        object_type: SnapshotObjectType::Page,
        object_number: 42,
        lines: vec![line(2569, "")],
    });
    let (result, _) = run_attach("run1-2-attach", TENANT, req).await;
    assert_eq!(result.kind, SnapshotAttachKind::UserSession);
    assert_eq!(result.affinity_cookie.as_deref(), Some(COOKIE));
}

#[tokio::test]
async fn attach_run2_background_session_5_without_user() {
    let mut req = request("00389080-795c-4af5-8c06-a347ac296d0d", vec![line(174, "")]);
    req.client_type = SnapshotClientType::Background;
    req.verbosity = SnapshotVerbosity::SnapPoint;
    req.session_id = 5;
    let (result, _) = run_attach("run2-2-attach", TENANT, req).await;
    assert_eq!(result.kind, SnapshotAttachKind::NextSessionOnTenant);
    assert_eq!(result.affinity_cookie.as_deref(), Some(COOKIE));
}

#[tokio::test]
async fn attach_run3_escapes_the_tenant_twice() {
    let mut req = request("55e1be1d-d794-4420-8d16-1697ae2b9a0d", vec![line(174, "")]);
    req.client_type = SnapshotClientType::WebServiceClient;
    let (_, got) = run_attach("run3-2-attach", ODD_TENANT, req).await;
    assert!(got
        .url
        .query()
        .unwrap()
        .ends_with("tenant=my%2520tenant%252Fx%252By"));
}

#[tokio::test]
async fn attach_run9_zero_values_are_left_out() {
    let mut req = request("47a54b3a-a8c6-4cb7-8e35-d3aaafd56908", vec![line(0, "")]);
    req.client_type = SnapshotClientType::WebServiceClient;
    req.user_id = Some(USER.to_string());
    req.verbosity = SnapshotVerbosity::SnapPoint;
    req.session_id = 0;
    let (result, _) = run_attach("run9-2-attach", TENANT, req).await;
    assert_eq!(result.kind, SnapshotAttachKind::UserSession);
}

#[tokio::test]
async fn attach_unknown_kind_is_a_bad_response() {
    let mut rec = recording("run1-2-attach");
    rec.response_body = b"Surprise".to_vec();
    let server = serve(&rec).await;
    let err = snapshot_attach(
        &reqwest::Client::new(),
        &config_for(&server, TENANT),
        TOKEN,
        false,
        &request("ed75bfc5-a198-4fe6-ad81-54b95d34965a", vec![]),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, SnapshotDebuggerError::BadResponse { .. }));
    assert!(Uuid::parse_str(err.correlation_id()).is_ok());
}

#[tokio::test]
async fn status_run1_initialized_and_started() {
    for (name, want) in [
        ("run1-3-status", SnapshotStatus::Initialized),
        ("run1-4-status", SnapshotStatus::Started),
    ] {
        let rec = recording(name);
        let server = serve(&rec).await;
        let got = snapshot_status(
            &reqwest::Client::new(),
            &config_for(&server, TENANT),
            TOKEN,
            false,
            "ed75bfc5-a198-4fe6-ad81-54b95d34965a",
            Some(COOKIE),
        )
        .await
        .unwrap();
        assert_matches(&server, &rec, "status").await;
        assert_eq!(got.status, want);
    }
}

#[tokio::test]
async fn status_and_finish_404_give_the_not_found_variant() {
    for name in ["run3-3-status", "run3-4-finish"] {
        let rec = recording(name);
        let server = serve(&rec).await;
        let cfg = config_for(&server, ODD_TENANT);
        let ctx = "55e1be1d-d794-4420-8d16-1697ae2b9a0d";
        let http = reqwest::Client::new();
        let err = if name.contains("status") {
            snapshot_status(&http, &cfg, TOKEN, false, ctx, Some(COOKIE))
                .await
                .map(|_| ())
        } else {
            snapshot_finish(&http, &cfg, TOKEN, false, ctx, Some(COOKIE))
                .await
                .map(|_| ())
        }
        .unwrap_err();
        assert_matches(&server, &rec, name.split('-').next_back().unwrap()).await;
        assert!(
            matches!(err, SnapshotDebuggerError::NotFound { .. }),
            "{err}"
        );
        let text = err.to_string();
        assert!(
            text.contains("timed out") && text.contains("30 minutes"),
            "{text}"
        );
        assert!(Uuid::parse_str(err.correlation_id()).is_ok());
    }
}

#[tokio::test]
async fn finish_run1_returns_the_zip_bytes() {
    let rec = recording("run1-5-finish");
    let server = serve(&rec).await;
    let got = snapshot_finish(
        &reqwest::Client::new(),
        &config_for(&server, TENANT),
        TOKEN,
        false,
        "ed75bfc5-a198-4fe6-ad81-54b95d34965a",
        Some(COOKIE),
    )
    .await
    .unwrap();
    assert_matches(&server, &rec, "finish").await;
    assert_eq!(got.zip.as_deref(), Some(rec.response_body.as_slice()));
    assert!(!got.sampling);
}

#[tokio::test]
async fn finish_run2_empty_body_means_nothing_was_recorded() {
    let rec = recording("run2-3-finish");
    let server = serve(&rec).await;
    let got = snapshot_finish(
        &reqwest::Client::new(),
        &config_for(&server, TENANT),
        TOKEN,
        false,
        "00389080-795c-4af5-8c06-a347ac296d0d",
        Some(COOKIE),
    )
    .await
    .unwrap();
    assert_matches(&server, &rec, "finish").await;
    assert_eq!(got.zip, None);
}

#[tokio::test]
async fn finish_run4_sampling_etag_sets_the_flag() {
    let rec = recording("run4-3-finish");
    let server = serve(&rec).await;
    let got = snapshot_finish(
        &reqwest::Client::new(),
        &config_for(&server, TENANT),
        TOKEN,
        false,
        "c4fbb573-e325-443c-9887-8d0574e833ee",
        Some(COOKIE),
    )
    .await
    .unwrap();
    assert_matches(&server, &rec, "finish").await;
    assert!(got.sampling);
    assert!(got.zip.is_some());
}

#[tokio::test]
async fn status_without_a_cookie_leaves_out_parameter_and_header() {
    let rec = recording("run1-3-status");
    let server = serve(&rec).await;
    snapshot_status(
        &reqwest::Client::new(),
        &config_for(&server, TENANT),
        TOKEN,
        false,
        "ed75bfc5-a198-4fe6-ad81-54b95d34965a",
        None,
    )
    .await
    .unwrap();
    let got = &server.received_requests().await.unwrap()[0];
    assert_eq!(
        got.url.query().unwrap(),
        format!("debuggingcontext=ed75bfc5-a198-4fe6-ad81-54b95d34965a&tenant={TENANT}")
    );
    assert!(got.headers.get("cookie").is_none());
}

#[tokio::test]
async fn session_telemetry_id_is_shared_and_activity_id_is_new_per_request() {
    let rec = recording("run1-3-status");
    let server = serve(&rec).await;
    let cfg = config_for(&server, TENANT);
    let http = reqwest::Client::new();
    for _ in 0..2 {
        snapshot_status(
            &http,
            &cfg,
            TOKEN,
            false,
            "ed75bfc5-a198-4fe6-ad81-54b95d34965a",
            None,
        )
        .await
        .unwrap();
    }
    let got = server.received_requests().await.unwrap();
    let h = |i: usize, n: &str| got[i].headers.get(n).unwrap().to_str().unwrap().to_string();
    assert_eq!(h(0, SESSION_ID_HEADER), h(1, SESSION_ID_HEADER));
    assert_ne!(h(0, ACTIVITY_ID_HEADER), h(1, ACTIVITY_ID_HEADER));
}

#[test]
fn snapshot_base_url_saas_escapes_the_environment_name() {
    let cfg = BcDebugConfig {
        environment_type: "Sandbox".into(),
        environment_name: Some("Sandbox".into()),
        ..BcDebugConfig::default()
    };
    assert_eq!(
        cfg.snapshot_base_url(),
        "https://api.businesscentral.dynamics.com/v2.0/Sandbox/snapshotdebugger"
    );
    let spaced = BcDebugConfig {
        environment_name: Some("My Env".into()),
        ..cfg
    };
    assert_eq!(
        spaced.snapshot_base_url(),
        "https://api.businesscentral.dynamics.com/v2.0/My%20Env/snapshotdebugger"
    );
}

#[test]
fn snapshot_base_url_onprem_defaults_to_port_7083_and_accepts_an_override() {
    let cfg = BcDebugConfig {
        environment_type: "OnPrem".into(),
        server: Some("http://bc.example".into()),
        server_instance: Some("BC240".into()),
        port: 7049,
        ..BcDebugConfig::default()
    };
    assert_eq!(
        cfg.snapshot_base_url(),
        "http://bc.example:7083/BC240/snapshotdebugger"
    );
    let custom = BcDebugConfig {
        snapshot_port: Some(9000),
        ..cfg
    };
    assert_eq!(
        custom.snapshot_base_url(),
        "http://bc.example:9000/BC240/snapshotdebugger"
    );
}

#[test]
fn snapshot_port_is_read_from_the_launch_arguments() {
    let cfg = BcDebugConfig::from_dap_args(&serde_json::json!({ "snapshotPort": 9000 }));
    assert_eq!(cfg.snapshot_port, Some(9000));
    let bad = BcDebugConfig::from_dap_args(&serde_json::json!({ "snapshotPort": 70000 }));
    assert!(!bad.validation_errors.is_empty());
}

#[tokio::test]
async fn production_is_refused_unless_allowed() {
    let rec = recording("run1-3-status");
    let server = serve(&rec).await;
    let http = reqwest::Client::new();
    let ctx = "ed75bfc5-a198-4fe6-ad81-54b95d34965a";
    for (kind, name) in [("production", None), ("Sandbox", Some("PRODUCTION"))] {
        let cfg = BcDebugConfig {
            environment_type: kind.into(),
            environment_name: name.map(String::from),
            ..config_for(&server, TENANT)
        };
        let err = snapshot_status(&http, &cfg, TOKEN, false, ctx, None)
            .await
            .unwrap_err();
        assert!(matches!(err, SnapshotDebuggerError::Production));
    }
    assert!(server.received_requests().await.unwrap().is_empty());
    let cfg = BcDebugConfig {
        environment_name: Some("Production".into()),
        ..config_for(&server, TENANT)
    };
    assert!(guard_production(&cfg, true).is_ok());
}

#[test]
fn new_debugging_context_is_a_guid() {
    let a = new_debugging_context();
    assert!(Uuid::parse_str(&a).is_ok());
    assert_ne!(a, new_debugging_context());
}
