//! The `snapshot` daemon method against a wiremock Business Central server.
//! The launch configuration is OnPrem with `snapshotPort` at the mock and the
//! requests carry an `accessToken`, so no real token path runs.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use al_bc::snapshot::state::{now_secs, SessionStore};
use al_protocol::jsonrpc::error_codes;
use serde_json::{json, Value};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::{dispatch_snapshot, TEST_AUTOMATION_BASE, TEST_STORE};

const TOKEN: &str = "FAKE-TEST-TOKEN-0123456789";
const COOKIE: &str = "abcdefghij0123456789AB%3D%3D";
const SYNTHETIC_ZIP: &[u8] = include_bytes!("testdata/synthetic-snapshot.zip");

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
    store: PathBuf,
    workspace: al_workspace::Workspace,
    server: MockServer,
}

async fn env_with(environment_type: &str) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap().join("project");
    std::fs::create_dir_all(root.join(".vscode")).unwrap();
    let server = MockServer::start().await;
    let port = server.address().port();
    let mut configuration = json!({
        "name": "Local",
        "type": "al",
        "request": "launch",
        "environmentType": environment_type,
        "server": "http://127.0.0.1",
        "serverInstance": "BC",
        "authentication": "AAD",
        "tenant": "default",
        "snapshotPort": port,
    });
    if environment_type != "OnPrem" {
        configuration["environmentName"] = json!("Live");
    }
    std::fs::write(
        root.join(".vscode/launch.json"),
        json!({ "configurations": [configuration] }).to_string(),
    )
    .unwrap();
    let store = dir.path().join("store");
    TEST_STORE.with(|cell| *cell.borrow_mut() = Some(store.clone()));
    TEST_AUTOMATION_BASE.with(|cell| *cell.borrow_mut() = None);
    let workspace = al_workspace::Workspace::new();
    crate::server::daemon::set_test_project_root(&workspace, &root);
    Env {
        _dir: dir,
        root,
        store,
        workspace,
        server,
    }
}

async fn env() -> Env {
    env_with("OnPrem").await
}

async fn call(env: &Env, params: Value) -> al_protocol::jsonrpc::Response {
    dispatch_snapshot(&env.workspace, 7, &params).await
}

async fn ok(env: &Env, params: Value) -> Value {
    let response = call(env, params).await;
    if let Some(error) = &response.error {
        panic!("snapshot call failed: {error:?}");
    }
    response.result.unwrap()
}

async fn err(env: &Env, params: Value) -> (i32, String) {
    let response = call(env, params).await;
    let error = response.error.expect("an error");
    (error.code, error.message)
}

async fn mock_attach(env: &Env, kind: &str) {
    Mock::given(method("POST"))
        .and(path("/BC/snapshotdebugger/attach"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(
                    "Set-Cookie",
                    format!("ApplicationGatewayAffinity={COOKIE}; Path=/"),
                )
                .set_body_string(kind),
        )
        .mount(&env.server)
        .await;
}

async fn mock_op(env: &Env, op: &str, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path(format!("/BC/snapshotdebugger/{op}")))
        .respond_with(response)
        .mount(&env.server)
        .await;
}

fn start_params() -> Value {
    json!({
        "cmd": "start",
        "config": "Local",
        "snaps": ["codeunit:50100:10?Amount > 1"],
        "breakOnNext": "WebClient",
        "accessToken": TOKEN,
    })
}

async fn requests(env: &Env) -> Vec<Request> {
    env.server.received_requests().await.unwrap()
}

fn store_text(store: &Path) -> String {
    let mut text = String::new();
    for entry in std::fs::read_dir(store).unwrap() {
        text.push_str(&std::fs::read_to_string(entry.unwrap().path()).unwrap());
    }
    text
}

#[tokio::test]
async fn start_attaches_and_saves_a_record_without_the_token() {
    let env = env().await;
    mock_attach(&env, "NextSessionOnTenant").await;
    let result = ok(&env, start_params()).await;
    assert_eq!(result["attachKind"], "NextSessionOnTenant");
    let context = result["debuggingContext"].as_str().unwrap();
    assert!(al_bc::bc_client::is_guid(context));
    let point = &result["snappoints"][0];
    assert_eq!(point["objectType"], "Codeunit");
    assert_eq!(point["id"], 50100);
    assert_eq!(point["line"], 10);
    assert_eq!(point["condition"], "Amount > 1");
    assert_eq!(point["origin"], "object id");
    // The mock is OnPrem and no automation base is set, so the check is skipped with a note.
    assert!(result["warnings"][0]
        .as_str()
        .unwrap()
        .contains("not checked"));

    let sent = requests(&env).await;
    assert_eq!(sent.len(), 1);
    let body = String::from_utf8_lossy(&sent[0].body).to_string();
    assert!(body.contains("\"ObjectNumber\":50100"), "{body}");
    assert!(body.contains("\"Line\":9"), "{body}");
    assert!(body.contains("Amount > 1"), "{body}");
    assert_eq!(
        sent[0]
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap(),
        format!("Bearer {TOKEN}")
    );

    let saved = SessionStore::at(&env.store).load(context).unwrap().unwrap();
    assert_eq!(saved.affinity_cookie.as_deref(), Some(COOKIE));
    assert_eq!(saved.configuration_name.as_deref(), Some("Local"));
    assert_eq!(saved.client_type, "WebClient");
    assert_eq!(saved.snappoints[0].line0, 9);
    assert_eq!(saved.project_root, env.root);
    assert!(!store_text(&env.store).contains(TOKEN), "token was stored");
}

#[tokio::test]
async fn start_validates_its_parameters() {
    let env = env().await;
    for (patch, needle) in [
        (json!({ "breakOnNext": null }), "breakOnNext"),
        (json!({ "breakOnNext": "Sometimes" }), "breakOnNext"),
        (json!({ "snaps": ["codeunit:22"] }), "line"),
        (json!({ "verbosity": "loud" }), "verbosity"),
        (json!({ "sessionId": -3 }), "sessionId"),
        (json!({ "config": "Nope" }), "not found"),
    ] {
        let mut params = start_params();
        for (key, value) in patch.as_object().unwrap() {
            params[key] = value.clone();
        }
        let (code, message) = err(&env, params).await;
        assert_eq!(code, error_codes::INVALID_PARAMS, "{message}");
        assert!(message.contains(needle), "{needle}: {message}");
    }
    assert!(requests(&env).await.is_empty());
}

#[tokio::test]
async fn production_is_refused_unless_allowed() {
    let env = env_with("Production").await;
    let (code, message) = err(&env, start_params()).await;
    assert_eq!(code, error_codes::INVALID_PARAMS);
    assert!(message.contains("allow_production"), "{message}");
    assert!(requests(&env).await.is_empty());
    assert!(!env.store.exists());
}

#[tokio::test]
async fn status_reports_seconds_since_started_and_the_eight_minute_warning() {
    let env = env().await;
    mock_attach(&env, "UserSession").await;
    let context = ok(&env, start_params()).await["debuggingContext"]
        .as_str()
        .unwrap()
        .to_string();
    mock_op(
        &env,
        "status",
        ResponseTemplate::new(200).set_body_string("Started"),
    )
    .await;

    let first = ok(&env, json!({ "cmd": "status", "accessToken": TOKEN })).await;
    assert_eq!(first["debuggingContext"], context.as_str());
    assert_eq!(first["status"], "Started");
    assert_eq!(first["secondsSinceStarted"], 0);
    assert_eq!(first["warnings"].as_array().unwrap().len(), 0);

    let store = SessionStore::at(&env.store);
    let mut record = store.load(&context).unwrap().unwrap();
    assert_eq!(record.last_status.as_deref(), Some("Started"));
    record.started_at = Some(now_secs() - 500);
    store.save(&record).unwrap();
    let late = ok(
        &env,
        json!({ "cmd": "status", "context": context, "accessToken": TOKEN }),
    )
    .await;
    assert!(late["secondsSinceStarted"].as_u64().unwrap() >= 500);
    let warning = late["warnings"][0].as_str().unwrap();
    assert!(warning.contains("10 minutes"), "{warning}");

    let sent = requests(&env).await;
    let status_request = sent.last().unwrap();
    let query = status_request.url.query().unwrap();
    assert!(
        query.contains("applicationgatewayaffinity=abcdefghij0123456789AB%253D%253D"),
        "{query}"
    );
    assert!(!store_text(&env.store).contains(TOKEN));
}

#[tokio::test]
async fn status_refuses_when_the_configuration_now_points_elsewhere() {
    let env = env().await;
    mock_attach(&env, "UserSession").await;
    ok(&env, start_params()).await;
    let launch = env.root.join(".vscode/launch.json");
    let edited = std::fs::read_to_string(&launch)
        .unwrap()
        .replace("\"default\"", "\"other-tenant\"");
    std::fs::write(&launch, edited).unwrap();
    let (code, message) = err(&env, json!({ "cmd": "status", "accessToken": TOKEN })).await;
    assert_eq!(code, error_codes::INVALID_PARAMS);
    assert!(
        message.contains("tenant or environment changed"),
        "{message}"
    );
    assert_eq!(requests(&env).await.len(), 1, "only the attach was sent");
}

#[tokio::test]
async fn status_without_a_session_asks_for_start() {
    let env = env().await;
    let (code, message) = err(&env, json!({ "cmd": "status", "accessToken": TOKEN })).await;
    assert_eq!(code, error_codes::INVALID_PARAMS);
    assert!(message.contains("snapshot start"), "{message}");
}

#[tokio::test]
async fn finish_writes_into_the_snapshots_folder_and_updates_the_record() {
    let env = env().await;
    mock_attach(&env, "UserSession").await;
    let context = ok(&env, start_params()).await["debuggingContext"]
        .as_str()
        .unwrap()
        .to_string();
    mock_op(
        &env,
        "finish",
        ResponseTemplate::new(200).set_body_bytes(SYNTHETIC_ZIP.to_vec()),
    )
    .await;
    let result = ok(&env, json!({ "cmd": "finish", "accessToken": TOKEN })).await;
    assert_eq!(result["empty"], false);
    assert_eq!(result["bytes"], SYNTHETIC_ZIP.len());
    assert!(result["cpuProfilePath"].is_null());
    let zip = PathBuf::from(result["zipPath"].as_str().unwrap());
    assert_eq!(
        zip,
        env.root.join(".snapshots").join(format!("{context}.zip"))
    );
    assert_eq!(std::fs::read(&zip).unwrap(), SYNTHETIC_ZIP);
    let record = SessionStore::at(&env.store)
        .load(&context)
        .unwrap()
        .unwrap();
    assert_eq!(record.zip_path.as_deref(), Some(zip.as_path()));
    assert!(record.finished_at.is_some());

    // The saved zip reads back through show.
    let shown = ok(
        &env,
        json!({ "cmd": "show", "path": zip.to_str().unwrap() }),
    )
    .await;
    assert!(shown["text"].as_str().unwrap().contains("== Stop 1"));
}

#[tokio::test]
async fn finish_to_a_folder_inside_the_project_and_refusing_one_outside() {
    let env = env().await;
    mock_attach(&env, "UserSession").await;
    ok(&env, start_params()).await;
    mock_op(
        &env,
        "finish",
        ResponseTemplate::new(200).set_body_bytes(SYNTHETIC_ZIP.to_vec()),
    )
    .await;
    let outside = env.root.parent().unwrap().join("elsewhere");
    let (code, message) = err(
        &env,
        json!({ "cmd": "finish", "outputDir": outside.to_str().unwrap(), "accessToken": TOKEN }),
    )
    .await;
    assert_eq!(code, error_codes::PATH_NOT_AUTHORIZED);
    assert!(message.contains("outside the project"), "{message}");
    assert!(!outside.exists());
    assert_eq!(requests(&env).await.len(), 1, "no finish request was sent");

    let inside = env.root.join("captures");
    let result = ok(
        &env,
        json!({ "cmd": "finish", "outputDir": inside.to_str().unwrap(), "accessToken": TOKEN }),
    )
    .await;
    assert!(result["zipPath"]
        .as_str()
        .unwrap()
        .starts_with(inside.to_str().unwrap()));
}

#[tokio::test]
async fn finish_takes_a_relative_output_folder_from_the_project_root() {
    let env = env().await;
    mock_attach(&env, "UserSession").await;
    ok(&env, start_params()).await;
    mock_op(
        &env,
        "finish",
        ResponseTemplate::new(200).set_body_bytes(SYNTHETIC_ZIP.to_vec()),
    )
    .await;
    let (code, message) = err(
        &env,
        json!({ "cmd": "finish", "outputDir": "../elsewhere", "accessToken": TOKEN }),
    )
    .await;
    assert_eq!(code, error_codes::PATH_NOT_AUTHORIZED);
    assert!(message.contains("outside the project"), "{message}");
    let result = ok(
        &env,
        json!({ "cmd": "finish", "outputDir": "relative-captures", "accessToken": TOKEN }),
    )
    .await;
    let inside = env.root.join("relative-captures");
    assert!(result["zipPath"]
        .as_str()
        .unwrap()
        .starts_with(inside.to_str().unwrap()));
}

#[tokio::test]
async fn an_empty_finish_says_nothing_was_recorded() {
    let env = env().await;
    mock_attach(&env, "UserSession").await;
    ok(&env, start_params()).await;
    mock_op(&env, "finish", ResponseTemplate::new(200)).await;
    let result = ok(&env, json!({ "cmd": "finish", "accessToken": TOKEN })).await;
    assert_eq!(result["empty"], true);
    assert!(result["zipPath"].is_null());
    let message = result["message"].as_str().unwrap();
    assert!(message.contains("nothing hit a snappoint"), "{message}");
    assert!(!env.root.join(".snapshots").exists());
}

#[tokio::test]
async fn a_404_on_finish_gives_the_not_found_message_with_the_correlation_id() {
    let env = env().await;
    mock_attach(&env, "UserSession").await;
    ok(&env, start_params()).await;
    mock_op(&env, "finish", ResponseTemplate::new(404)).await;
    let (_, message) = err(&env, json!({ "cmd": "finish", "accessToken": TOKEN })).await;
    assert!(
        message.contains("no longer knows this debugging context"),
        "{message}"
    );
    assert!(message.contains("(request "), "{message}");
}

#[tokio::test]
async fn list_answers_the_records_without_cookies() {
    let env = env().await;
    mock_attach(&env, "UserSession").await;
    let first = ok(&env, start_params()).await["debuggingContext"]
        .as_str()
        .unwrap()
        .to_string();
    let result = ok(&env, json!({ "cmd": "list" })).await;
    let sessions = result["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["debuggingContext"], first.as_str());
    assert!(sessions[0].get("affinityCookie").is_none());
    assert!(!result.to_string().contains(COOKIE));
    let empty_env = Env {
        store: env.store.join("none"),
        ..env
    };
    TEST_STORE.with(|cell| *cell.borrow_mut() = Some(empty_env.store.clone()));
    let none = ok(&empty_env, json!({ "cmd": "list" })).await;
    assert_eq!(none["sessions"].as_array().unwrap().len(), 0);
}

fn project_zip(env: &Env) -> PathBuf {
    let path = env.root.join("sample.zip");
    std::fs::write(&path, SYNTHETIC_ZIP).unwrap();
    path
}

#[tokio::test]
async fn show_renders_text_filters_and_json() {
    let env = env().await;
    let zip = project_zip(&env);
    let path = zip.to_str().unwrap();

    let all = ok(&env, json!({ "cmd": "show", "path": path })).await;
    let text = all["text"].as_str().unwrap();
    assert!(
        text.contains("== Stop 1: Page 42 \"Sales Order\" line 2569"),
        "{text}"
    );
    assert!(text.contains("Error capture 2"), "{text}");

    let one = ok(
        &env,
        json!({ "cmd": "show", "path": path, "stop": 1, "vars": "Rec", "fields": ["Name", "Amount"] }),
    )
    .await;
    let text = one["text"].as_str().unwrap();
    assert!(
        !text.contains("Error capture") && !text.contains("Locals:"),
        "{text}"
    );
    assert!(text.contains("Name = 'Stark'"), "{text}");

    let as_json = ok(
        &env,
        json!({ "cmd": "show", "path": path, "json": true, "stop": 2 }),
    )
    .await;
    let stops = as_json["stops"].as_array().unwrap();
    assert_eq!(stops.len(), 1);
    assert_eq!(stops[0]["kind"], "error");

    let (code, _) = err(&env, json!({ "cmd": "show" })).await;
    assert_eq!(code, error_codes::INVALID_PARAMS);
    let junk = env.root.join("junk.zip");
    std::fs::write(&junk, b"not a zip").unwrap();
    let (code, message) = err(
        &env,
        json!({ "cmd": "show", "path": junk.to_str().unwrap() }),
    )
    .await;
    assert_eq!(code, error_codes::INVALID_PARAMS);
    assert!(message.contains("cannot read"), "{message}");
}

#[tokio::test]
async fn unknown_and_missing_commands_are_invalid_params() {
    let env = env().await;
    let (code, message) = err(&env, json!({ "cmd": "download" })).await;
    assert_eq!(code, error_codes::INVALID_PARAMS);
    assert!(message.contains("download"), "{message}");
    let (code, _) = err(&env, json!({})).await;
    assert_eq!(code, error_codes::INVALID_PARAMS);
}

async fn mock_automation(env: &Env, base_app: Value) -> String {
    let company = "11111111-aaaa-bbbb-cccc-000000000001";
    let get = |p: String| Mock::given(method("GET")).and(path(p));
    get("/companies".to_string())
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "value": [{ "id": company }] })),
        )
        .mount(&env.server)
        .await;
    get(format!("/companies({company})/users"))
        .and(query_param("$filter", "userName eq 'ALICE'"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({ "value": [{ "userSecurityId": "aaaaaaaa-0000-0000-0000-00000000000a" }] }),
        ))
        .mount(&env.server)
        .await;
    get(format!("/companies({company})/extensions"))
        .respond_with(base_app_response(base_app))
        .mount(&env.server)
        .await;
    env.server.uri()
}

fn base_app_response(body: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(body)
}

fn write_base_app(dir: &Path, version: &str) {
    let manifest = format!(
        r#"<?xml version="1.0" encoding="utf-8"?><Package><App Id="id-base" Name="Base Application" Publisher="Microsoft" Version="{version}" /></Package>"#
    );
    let mut data = b"NAVX".to_vec();
    data.extend_from_slice(&1u32.to_le_bytes());
    data.extend_from_slice(&[0u8; 32]);
    let mut buffer = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
        zip.start_file("NavxManifest.xml", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(manifest.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    data.extend_from_slice(&buffer);
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("Microsoft_Base Application.app"), data).unwrap();
}

#[tokio::test]
async fn a_user_name_is_resolved_and_a_base_app_mismatch_warns() {
    let env = env().await;
    mock_attach(&env, "NextSessionForUserOnTenant").await;
    let base = mock_automation(
        &env,
        json!({ "value": [{ "versionMajor": 29, "versionMinor": 0, "versionBuild": 111, "versionRevision": 2 }] }),
    )
    .await;
    TEST_AUTOMATION_BASE.with(|cell| *cell.borrow_mut() = Some(base));
    write_base_app(&env.root.join(".alpackages"), "28.0.0.0");

    let mut params = start_params();
    params["userId"] = json!("ALICE");
    let result = ok(&env, params).await;
    let warning = result["warnings"][0].as_str().unwrap();
    assert!(warning.contains("BaseApp 29.0.111.2"), "{warning}");
    assert!(
        warning.contains(".alpackages BaseApp is 28.0.0.0"),
        "{warning}"
    );

    let attach = requests(&env)
        .await
        .into_iter()
        .find(|r| r.url.path().ends_with("/attach"))
        .unwrap();
    let query = attach.url.query().unwrap();
    assert!(
        query.contains("userid=aaaaaaaa-0000-0000-0000-00000000000a"),
        "{query}"
    );
    let body = String::from_utf8_lossy(&attach.body).to_string();
    assert!(
        body.contains("\"UserId\":\"aaaaaaaa-0000-0000-0000-00000000000a\""),
        "{body}"
    );
    let record = SessionStore::at(&env.store)
        .load(result["debuggingContext"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        record.user_id.as_deref(),
        Some("aaaaaaaa-0000-0000-0000-00000000000a")
    );
}

#[tokio::test]
async fn a_failed_version_lookup_is_a_warning_and_an_unknown_user_is_an_error() {
    let env = env().await;
    mock_attach(&env, "NextSessionForUserOnTenant").await;
    let base = mock_automation(&env, json!({ "value": [] })).await;
    TEST_AUTOMATION_BASE.with(|cell| *cell.borrow_mut() = Some(base));

    let result = ok(&env, start_params()).await;
    let warning = result["warnings"][0].as_str().unwrap();
    assert!(
        warning.contains("Base Application version not checked"),
        "{warning}"
    );

    let mut params = start_params();
    params["userId"] = json!("BOB");
    let (code, message) = err(&env, params).await;
    assert_eq!(code, error_codes::INVALID_PARAMS);
    assert!(message.contains("BOB"), "{message}");
}

#[tokio::test]
async fn start_defaults_to_snappoint_verbosity_and_needs_no_snappoints() {
    let env = env().await;
    mock_attach(&env, "NextSessionOnTenant").await;
    let mut params = start_params();
    params.as_object_mut().unwrap().remove("snaps");
    let result = ok(&env, params).await;
    assert_eq!(result["snappoints"], json!([]));
    let sent = requests(&env).await;
    let body = String::from_utf8_lossy(&sent[0].body).to_string();
    // SnapPoint is 0, so the field is omitted. No recording covers an empty
    // list, so it is sent as [].
    assert!(!body.contains("SnapshotVerbosity"), "{body}");
    assert!(body.contains("\"SourceBreakpointLocations\":[]"), "{body}");
    let context = result["debuggingContext"].as_str().unwrap();
    let saved = SessionStore::at(&env.store).load(context).unwrap().unwrap();
    assert_eq!(saved.verbosity, "SnapPoint");
}

#[tokio::test]
async fn full_verbosity_is_sent_when_asked_for() {
    let env = env().await;
    mock_attach(&env, "NextSessionOnTenant").await;
    let mut params = start_params();
    params["verbosity"] = json!("full");
    ok(&env, params).await;
    let body = String::from_utf8_lossy(&requests(&env).await[0].body).to_string();
    assert!(body.contains("\"SnapshotVerbosity\":1"), "{body}");
}

#[tokio::test]
async fn finish_writes_a_gitignore_into_the_default_dir_it_creates() {
    let env = env().await;
    mock_attach(&env, "UserSession").await;
    ok(&env, start_params()).await;
    mock_op(
        &env,
        "finish",
        ResponseTemplate::new(200).set_body_bytes(SYNTHETIC_ZIP.to_vec()),
    )
    .await;
    ok(&env, json!({ "cmd": "finish", "accessToken": TOKEN })).await;
    let ignore = env.root.join(".snapshots/.gitignore");
    assert_eq!(std::fs::read_to_string(ignore).unwrap(), "*\n");
}

#[cfg(unix)]
#[tokio::test]
async fn a_default_snapshots_dir_that_links_out_of_the_project_is_refused() {
    let env = env().await;
    mock_attach(&env, "UserSession").await;
    ok(&env, start_params()).await;
    mock_op(
        &env,
        "finish",
        ResponseTemplate::new(200).set_body_bytes(SYNTHETIC_ZIP.to_vec()),
    )
    .await;
    let outside = env.root.parent().unwrap().join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, env.root.join(".snapshots")).unwrap();
    let (_, message) = err(&env, json!({ "cmd": "finish", "accessToken": TOKEN })).await;
    assert!(message.contains("outside the project"), "{message}");
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    assert_eq!(requests(&env).await.len(), 1, "no finish request was sent");
}
