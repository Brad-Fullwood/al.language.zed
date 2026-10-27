//! Strict, environment-gated Business Central integration contract.
//!
//! This test is ignored in the self-contained suite and is run only by
//! `make live-bc-contracts`. Missing inputs are not a pass: the Make target
//! exits 2 before Cargo starts, and a direct ignored run reports UNAVAILABLE.

use std::collections::VecDeque;
use std::error::Error;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use al_bc::launch::{find_launch_config, AuthMethod, BcServerConfig};
use al_test::test_runner::TestRunnerClient;
use al_test_harness::{al_explorer_binary, al_lsp_binary};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout, Command as TokioCommand};
use tokio::time::timeout;

type AnyResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

fn failure(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}

fn required_env(name: &str) -> AnyResult<String> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(failure(format!(
            "UNAVAILABLE: required live-BC input {name} is missing or blank"
        ))
        .into()),
    }
}

fn timeout_duration() -> AnyResult<Duration> {
    let seconds = std::env::var("AL_LIVE_BC_TIMEOUT_SECS")
        .unwrap_or_else(|_| "300".to_string())
        .parse::<u64>()
        .map_err(|error| failure(format!("AL_LIVE_BC_TIMEOUT_SECS is invalid: {error}")))?;
    if seconds == 0 || seconds > 3600 {
        return Err(failure("AL_LIVE_BC_TIMEOUT_SECS must be from 1 through 3600").into());
    }
    Ok(Duration::from_secs(seconds))
}

fn selected_server_config(project: &Path, name: &str) -> AnyResult<BcServerConfig> {
    let launch = find_launch_config(project)?
        .ok_or_else(|| failure("UNAVAILABLE: project has no AL debug configuration"))?;
    launch
        .configs
        .into_iter()
        .find(|config| config.name == name)
        .ok_or_else(|| {
            failure(format!(
                "UNAVAILABLE: debug configuration {name:?} was not found"
            ))
            .into()
        })
}

fn breakpoint_path(project: &Path, configured: &str) -> AnyResult<PathBuf> {
    let path = Path::new(configured);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        project.join(path)
    };
    let canonical = path.canonicalize().map_err(|error| {
        failure(format!(
            "UNAVAILABLE: breakpoint file {} cannot be resolved: {error}",
            path.display()
        ))
    })?;
    if !canonical.starts_with(project) || !canonical.is_file() {
        return Err(failure(format!(
            "UNAVAILABLE: breakpoint file {} must be a regular file inside the project",
            canonical.display()
        ))
        .into());
    }
    Ok(canonical)
}

/// Run `al-explorer --json <args> --tenant <tenant>` in the project. Every call
/// types the tenant GUID, as the CLI requires for any command that may connect.
fn cli_json(contract: &LiveContract, args: &[&str]) -> AnyResult<Value> {
    let project = &contract.project;
    let tenant = contract.tenant()?;
    let deadline = contract.timeout_ms.to_string();
    let output = Command::new(al_explorer_binary())
        .arg("--json")
        .args(args)
        .args([
            "--tenant",
            tenant.as_str(),
            "--timeout-ms",
            deadline.as_str(),
        ])
        .current_dir(project)
        .output()
        .map_err(|error| failure(format!("run al-explorer {}: {error}", args.join(" "))))?;
    if !output.status.success() {
        return Err(failure(format!(
            "al-explorer {} failed with {:?}\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
        .into());
    }
    serde_json::from_slice(&output.stdout).map_err(|error| {
        failure(format!(
            "al-explorer {} returned invalid JSON: {error}\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
        .into()
    })
}

fn json_has_string(value: &Value, key: &str, expected: &str) -> bool {
    match value {
        Value::Object(object) => {
            object.get(key).and_then(Value::as_str) == Some(expected)
                || object
                    .values()
                    .any(|value| json_has_string(value, key, expected))
        }
        Value::Array(values) => values
            .iter()
            .any(|value| json_has_string(value, key, expected)),
        _ => false,
    }
}

fn json_has_u64(value: &Value, key: &str, expected: u64) -> bool {
    match value {
        Value::Object(object) => {
            object.get(key).and_then(Value::as_u64) == Some(expected)
                || object
                    .values()
                    .any(|value| json_has_u64(value, key, expected))
        }
        Value::Array(values) => values
            .iter()
            .any(|value| json_has_u64(value, key, expected)),
        _ => false,
    }
}

struct LiveContract {
    project: PathBuf,
    config_name: String,
    codeunit_id: i32,
    codeunit_name: String,
    method: String,
    breakpoint_file: String,
    breakpoint: PathBuf,
    breakpoint_line: u32,
    expression: String,
    expected_expression: Option<String>,
    bc_version: String,
    operation_timeout: Duration,
    timeout_ms: u64,
    server: BcServerConfig,
}

impl LiveContract {
    /// The tenant GUID the launch configuration names.
    fn tenant(&self) -> AnyResult<String> {
        self.server
            .tenant
            .clone()
            .filter(|tenant| !tenant.trim().is_empty())
            .ok_or_else(|| failure("UNAVAILABLE: the launch configuration has no tenant").into())
    }

    fn from_env() -> AnyResult<Self> {
        let project = PathBuf::from(required_env("AL_LIVE_BC_PROJECT")?).canonicalize()?;
        if !project.join("app.json").is_file() {
            return Err(failure("UNAVAILABLE: AL_LIVE_BC_PROJECT has no app.json").into());
        }
        let config_name = required_env("AL_LIVE_BC_CONFIG")?;
        let codeunit_id = required_env("AL_LIVE_BC_TEST_CODEUNIT_ID")?
            .parse::<i32>()
            .map_err(|error| failure(format!("AL_LIVE_BC_TEST_CODEUNIT_ID is invalid: {error}")))?;
        let codeunit_name = required_env("AL_LIVE_BC_TEST_CODEUNIT_NAME")?;
        let method = required_env("AL_LIVE_BC_TEST_METHOD")?;
        let breakpoint_file = required_env("AL_LIVE_BC_BREAKPOINT_FILE")?;
        let breakpoint_line = required_env("AL_LIVE_BC_BREAKPOINT_LINE")?
            .parse::<u32>()
            .map_err(|error| failure(format!("AL_LIVE_BC_BREAKPOINT_LINE is invalid: {error}")))?;
        if breakpoint_line == 0 {
            return Err(failure("AL_LIVE_BC_BREAKPOINT_LINE must be 1-based").into());
        }
        let expression = required_env("AL_LIVE_BC_EVAL")?;
        let expected_expression = std::env::var("AL_LIVE_BC_EXPECT_EVAL").ok();
        let bc_version = required_env("AL_LIVE_BC_VERSION")?;
        let operation_timeout = timeout_duration()?;
        let timeout_ms = u64::try_from(operation_timeout.as_millis())
            .map_err(|_| failure("live BC timeout does not fit milliseconds"))?;
        let breakpoint = breakpoint_path(&project, &breakpoint_file)?;
        let server = selected_server_config(&project, &config_name)?;
        if server.authentication != AuthMethod::AAD {
            return Err(failure(format!(
                "UNAVAILABLE: native live profile requires AAD/MicrosoftEntraID; config uses {:?}",
                server.authentication
            ))
            .into());
        }
        al_bc::http_auth::access_token_from_env()?
            .ok_or_else(|| failure("UNAVAILABLE: BC_ACCESS_TOKEN or BC_TOKEN is required"))?;

        Ok(Self {
            project,
            config_name,
            codeunit_id,
            codeunit_name,
            method,
            breakpoint_file,
            breakpoint,
            breakpoint_line,
            expression,
            expected_expression,
            bc_version,
            operation_timeout,
            timeout_ms,
            server,
        })
    }
}

struct DaemonGuard {
    project: PathBuf,
}

impl DaemonGuard {
    fn reset(project: &Path) -> Self {
        let _ = Command::new(al_explorer_binary())
            .args(["--json", "daemon-shutdown"])
            .current_dir(project)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        Self {
            project: project.to_path_buf(),
        }
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = Command::new(al_explorer_binary())
            .args(["--json", "daemon-shutdown"])
            .current_dir(&self.project)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

struct DapClient {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    stderr_task: tokio::task::JoinHandle<Vec<u8>>,
    pending: VecDeque<Value>,
    next_seq: i64,
    operation_timeout: Duration,
}

impl Drop for DapClient {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        self.stderr_task.abort();
    }
}

impl DapClient {
    async fn spawn(project: &Path, operation_timeout: Duration) -> AnyResult<Self> {
        let mut child = TokioCommand::new(al_lsp_binary())
            .arg("--dap")
            .current_dir(project)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| failure("DAP child has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| failure("DAP child has no stdout"))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| failure("DAP child has no stderr"))?;
        let stderr_task = tokio::spawn(async move {
            let mut bytes = Vec::new();
            let _ = stderr.read_to_end(&mut bytes).await;
            bytes
        });
        Ok(Self {
            child,
            stdin,
            stdout,
            stderr_task,
            pending: VecDeque::new(),
            next_seq: 1,
            operation_timeout,
        })
    }

    async fn send_request(&mut self, command: &str, arguments: Value) -> AnyResult<i64> {
        let seq = self.next_seq;
        self.next_seq += 1;
        let message = json!({
            "seq": seq,
            "type": "request",
            "command": command,
            "arguments": arguments,
        });
        let body = serde_json::to_vec(&message)?;
        self.stdin
            .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
            .await?;
        self.stdin.write_all(&body).await?;
        self.stdin.flush().await?;
        Ok(seq)
    }

    async fn read_message(&mut self) -> AnyResult<Value> {
        const MAX_HEADER_BYTES: usize = 16 * 1024;
        const MAX_BODY_BYTES: usize = 20 * 1024 * 1024;
        let read = async {
            let mut header = Vec::new();
            loop {
                let mut byte = [0u8; 1];
                self.stdout.read_exact(&mut byte).await?;
                header.push(byte[0]);
                if header.len() > MAX_HEADER_BYTES {
                    return Err(failure("DAP response header exceeded 16 KiB").into());
                }
                if header.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let header = std::str::from_utf8(&header)?;
            let length = header
                .lines()
                .find_map(|line| line.strip_prefix("Content-Length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .ok_or_else(|| failure("DAP response omitted Content-Length"))?;
            if length > MAX_BODY_BYTES {
                return Err(failure("DAP response body exceeded 20 MiB").into());
            }
            let mut body = vec![0u8; length];
            self.stdout.read_exact(&mut body).await?;
            Ok(serde_json::from_slice(&body)?)
        };
        timeout(self.operation_timeout, read)
            .await
            .map_err(|_| failure("timed out waiting for DAP message"))?
    }

    async fn response(&mut self, request_seq: i64) -> AnyResult<Value> {
        if let Some(index) = self.pending.iter().position(|message| {
            message["type"] == "response" && message["request_seq"] == request_seq
        }) {
            return Ok(self.pending.remove(index).expect("pending response"));
        }
        loop {
            let message = self.read_message().await?;
            if message["type"] == "response" && message["request_seq"] == request_seq {
                return Ok(message);
            }
            self.pending.push_back(message);
        }
    }

    async fn event(&mut self, name: &str) -> AnyResult<Value> {
        if let Some(index) = self
            .pending
            .iter()
            .position(|message| message["type"] == "event" && message["event"] == name)
        {
            return Ok(self.pending.remove(index).expect("pending event"));
        }
        loop {
            let message = self.read_message().await?;
            if message["type"] == "event" && message["event"] == name {
                return Ok(message);
            }
            self.pending.push_back(message);
        }
    }

    async fn successful_request(&mut self, command: &str, arguments: Value) -> AnyResult<Value> {
        let seq = self.send_request(command, arguments).await?;
        let response = self.response(seq).await?;
        if response["success"] != true {
            return Err(failure(format!(
                "DAP {command} failed: {}",
                response["message"].as_str().unwrap_or("<no message>")
            ))
            .into());
        }
        Ok(response)
    }

    fn output_text(&self) -> String {
        self.pending
            .iter()
            .filter(|message| message["type"] == "event" && message["event"] == "output")
            .filter_map(|message| message["body"]["output"].as_str())
            .collect::<String>()
    }

    async fn disconnect_and_wait(&mut self) -> AnyResult<()> {
        self.successful_request("disconnect", json!({"terminateDebuggee": false}))
            .await?;
        timeout(Duration::from_secs(30), self.child.wait())
            .await
            .map_err(|_| failure("DAP process did not exit after disconnect"))??;
        Ok(())
    }
}

async fn publish_succeeds_on_http_status(project: &Path, config_name: &str) -> AnyResult<()> {
    let workspace = al_workspace::Workspace::new();
    *workspace.config.write().await = al_project::config::AlConfig::load_effective(project)?;
    al_workspace::initialize_core_workspace(&workspace, project).await?;
    let mut config = al_publish::PublishConfig::new(project);
    config.config_name = Some(config_name.to_string());
    let result = al_publish::publish(&workspace, &config).await?;
    if !result.success {
        return Err(failure(format!(
            "publish did not get an HTTP success status from dev/apps: {}",
            serde_json::to_string_pretty(&result)?
        ))
        .into());
    }
    if !result
        .steps
        .iter()
        .any(|step| step.phase == al_publish::PublishPhase::Upload && step.success)
    {
        return Err(failure("publish result omitted a successful upload/install step").into());
    }
    Ok(())
}

async fn exercise_native_dap(contract: &LiveContract) -> AnyResult<()> {
    let mut dap = DapClient::spawn(&contract.project, contract.operation_timeout).await?;
    let initialize = dap
        .successful_request(
            "initialize",
            json!({
                "adapterID": "al",
                "clientID": "live-bc-contract",
                "linesStartAt1": true,
                "columnsStartAt1": true,
                "pathFormat": "path",
            }),
        )
        .await?;
    if initialize["body"]["supportsConfigurationDoneRequest"] != true
        || initialize["body"]["supportsConditionalBreakpoints"] != true
    {
        return Err(failure(format!(
            "DAP initialize capabilities are incomplete: {initialize}"
        ))
        .into());
    }
    dap.event("initialized").await?;

    let mut launch_args = contract.server.debug_args.clone();
    let launch = launch_args
        .as_object_mut()
        .ok_or_else(|| failure("selected launch configuration is not a JSON object"))?;
    launch.insert("request".to_string(), json!("launch"));
    launch.insert("launchBrowser".to_string(), json!(false));
    launch.insert("breakOnNext".to_string(), json!("WebServiceClient"));
    dap.successful_request("launch", launch_args).await?;
    let launch_output = dap.output_text();
    for expected in [
        "Compilation succeeded.",
        "Package published successfully.",
        "Debug session started.",
    ] {
        if !launch_output.contains(expected) {
            return Err(failure(format!(
                "DAP launch output omitted {expected:?}:\n{launch_output}"
            ))
            .into());
        }
    }

    let breakpoints = dap
        .successful_request(
            "setBreakpoints",
            json!({
                "source": {"path": contract.breakpoint},
                "breakpoints": [{"line": contract.breakpoint_line}],
                "sourceModified": false,
            }),
        )
        .await?;
    if breakpoints["body"]["breakpoints"][0]["verified"] != true {
        return Err(failure(format!(
            "Business Central did not verify the requested breakpoint: {breakpoints}"
        ))
        .into());
    }
    dap.successful_request("configurationDone", json!({}))
        .await?;

    let runner = TestRunnerClient;
    let codeunit_name_owned = contract.codeunit_name.clone();
    let method_owned = contract.method.clone();
    let codeunit_id = contract.codeunit_id;
    let run = tokio::spawn(async move {
        runner
            .run_codeunit(codeunit_id, &codeunit_name_owned, Some(&method_owned))
            .await
    });

    let stopped = dap.event("stopped").await?;
    if stopped["body"]["threadId"] != 1 {
        return Err(failure(format!("DAP stopped event omitted thread 1: {stopped}")).into());
    }
    let threads = dap.successful_request("threads", json!({})).await?;
    if threads["body"]["threads"]
        .as_array()
        .is_none_or(Vec::is_empty)
    {
        return Err(failure(format!("DAP threads response is empty: {threads}")).into());
    }
    let stack = dap
        .successful_request("stackTrace", json!({"threadId": 1}))
        .await?;
    let frames = stack["body"]["stackFrames"]
        .as_array()
        .ok_or_else(|| failure(format!("DAP stackFrames is not an array: {stack}")))?;
    let frame_id = frames
        .first()
        .and_then(|frame| frame["id"].as_i64())
        .ok_or_else(|| failure(format!("DAP stack trace has no frame ID: {stack}")))?;
    let scopes = dap
        .successful_request("scopes", json!({"frameId": frame_id}))
        .await?;
    let locals_reference = scopes["body"]["scopes"]
        .as_array()
        .and_then(|scopes| {
            scopes.iter().find(|scope| {
                scope["name"]
                    .as_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case("locals"))
            })
        })
        .and_then(|scope| scope["variablesReference"].as_i64())
        .ok_or_else(|| failure(format!("DAP locals scope is missing: {scopes}")))?;
    let variables = dap
        .successful_request("variables", json!({"variablesReference": locals_reference}))
        .await?;
    if variables["body"]["variables"]
        .as_array()
        .is_none_or(Vec::is_empty)
    {
        return Err(failure(format!(
            "DAP locals are empty at the configured breakpoint: {variables}"
        ))
        .into());
    }
    let evaluated = dap
        .successful_request(
            "evaluate",
            json!({
                "expression": contract.expression,
                "frameId": frame_id,
                "context": "watch",
            }),
        )
        .await?;
    let evaluated_value = evaluated["body"]["result"]
        .as_str()
        .ok_or_else(|| failure(format!("DAP evaluate returned no result: {evaluated}")))?;
    if let Some(expected) = contract.expected_expression.as_deref() {
        if evaluated_value != expected {
            return Err(failure(format!(
                "DAP evaluate mismatch: expected {expected:?}, got {evaluated_value:?}"
            ))
            .into());
        }
    }

    dap.successful_request("next", json!({"threadId": 1}))
        .await?;
    dap.event("stopped").await?;
    dap.successful_request("continue", json!({"threadId": 1}))
        .await?;

    let test_result = timeout(contract.operation_timeout, run)
        .await
        .map_err(|_| failure("live BC test did not complete after continue"))???;
    if test_result.failed != 0 || test_result.passed == 0 {
        return Err(failure(format!(
            "live BC test failed after DAP stepping: {test_result:?}"
        ))
        .into());
    }
    dap.disconnect_and_wait().await?;
    Ok(())
}

fn exercise_cli_live_test_and_snapshots(contract: &LiveContract) -> AnyResult<()> {
    let _daemon = DaemonGuard::reset(&contract.project);
    let codeunit_id_string = contract.codeunit_id.to_string();
    let timeout_string = contract.timeout_ms.to_string();
    let live_run = cli_json(
        contract,
        &[
            "test-run",
            &codeunit_id_string,
            "--name",
            &contract.codeunit_name,
            "--method",
            &contract.method,
            "--config",
            &contract.config_name,
        ],
    )?;
    if !json_has_string(&live_run, "decision", "liveBc") || !json_has_u64(&live_run, "passed", 1) {
        return Err(failure(format!(
            "CLI test did not prove a passing live-BC route: {live_run}"
        ))
        .into());
    }

    let snapshots = tempfile::Builder::new()
        .prefix(".al-live-contract-")
        .tempdir_in(&contract.project)?;
    let snapshot = snapshots.path().join("capture.snap.json");
    let snapshot_string = snapshot.to_string_lossy().into_owned();
    let breakpoint = format!("{}:{}", contract.breakpoint_file, contract.breakpoint_line);
    let capture = cli_json(
        contract,
        &[
            "test-snapshot",
            "capture",
            &codeunit_id_string,
            &contract.codeunit_name,
            &contract.method,
            "--bc-version",
            &contract.bc_version,
            "--breakpoint",
            &breakpoint,
            "--output",
            &snapshot_string,
            "--config",
            &contract.config_name,
            "--timeout-ms",
            &timeout_string,
        ],
    )?;
    if capture["sampleCount"].as_u64().unwrap_or(0) == 0 || !snapshot.is_file() {
        return Err(failure(format!(
            "live snapshot capture produced no persisted samples: {capture}"
        ))
        .into());
    }
    let validated = cli_json(contract, &["test-snapshot", "validate", &snapshot_string])?;
    if validated["sampleCount"].as_u64().unwrap_or(0) == 0 {
        return Err(failure(format!(
            "captured snapshot did not validate with samples: {validated}"
        ))
        .into());
    }
    let replay = cli_json(
        contract,
        &[
            "test-snapshot",
            "replay",
            &snapshot_string,
            "--bc-version",
            &contract.bc_version,
            "--config",
            &contract.config_name,
            "--timeout-ms",
            &timeout_string,
        ],
    )?;
    if replay["divergences"]
        .as_array()
        .is_none_or(|divergences| !divergences.is_empty())
    {
        return Err(failure(format!(
            "live snapshot replay diverged from the capture: {replay}"
        ))
        .into());
    }
    let diff = cli_json(
        contract,
        &["test-snapshot", "diff", &snapshot_string, &snapshot_string],
    )?;
    if diff["divergences"]
        .as_array()
        .is_none_or(|divergences| !divergences.is_empty())
    {
        return Err(failure(format!("snapshot self-diff was not clean: {diff}")).into());
    }
    Ok(())
}

fn repository_live_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/live_bc_contract_project")
}

fn copy_fixture_tree(source: &Path, destination: &Path) -> io::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_fixture_tree(&source_path, &destination_path)?;
        } else {
            std::fs::copy(source_path, destination_path)?;
        }
    }
    Ok(())
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("al-test-harness must be inside <repository>/crates")
        .to_path_buf()
}

fn generated_fixture_preflight_with_bash_env(
    tenant: &str,
    bash_env: Option<&Path>,
) -> io::Result<std::process::Output> {
    let mut command = Command::new("bash");
    command
        .arg(repository_root().join("scripts/live-bc-contracts.sh"))
        .arg("--preflight-only")
        .current_dir(repository_root());
    for name in [
        "AL_LIVE_BC_PROJECT",
        "AL_LIVE_BC_CONFIG",
        "AL_LIVE_BC_TEST_CODEUNIT_ID",
        "AL_LIVE_BC_TEST_CODEUNIT_NAME",
        "AL_LIVE_BC_TEST_METHOD",
        "AL_LIVE_BC_BREAKPOINT_FILE",
        "AL_LIVE_BC_BREAKPOINT_LINE",
        "AL_LIVE_BC_EVAL",
        "AL_LIVE_BC_EXPECT_EVAL",
        "AL_LIVE_BC_TENANT",
        "AL_LIVE_BC_ENVIRONMENT",
        "AL_LIVE_BC_VERSION",
        "BC_ACCESS_TOKEN",
        "BC_TOKEN",
        "BASH_ENV",
    ] {
        command.env_remove(name);
    }
    if let Some(path) = bash_env {
        command.env("BASH_ENV", path);
    }
    command
        .env("AL_LIVE_BC_TENANT", tenant)
        .env("AL_LIVE_BC_ENVIRONMENT", "CDX Sandbox")
        .env("AL_LIVE_BC_VERSION", "26.5.0.0")
        .env("BC_ACCESS_TOKEN", "fixture-preflight-token")
        .output()
}

fn generated_fixture_preflight(tenant: &str) -> io::Result<std::process::Output> {
    generated_fixture_preflight_with_bash_env(tenant, None)
}

#[test]
fn repository_live_fixture_builds_and_routes_the_exact_test_to_live_bc() -> AnyResult<()> {
    let fixture = repository_live_fixture();
    let temp = tempfile::tempdir()?;
    copy_fixture_tree(&fixture, temp.path())?;
    let launch_template = std::fs::read_to_string(temp.path().join(".vscode/launch.json.in"))?;
    std::fs::write(
        temp.path().join(".vscode/launch.json"),
        launch_template
            .replace("@TENANT@", "demo.onmicrosoft.com")
            .replace("@ENVIRONMENT@", "CDX Sandbox"),
    )?;
    let server = selected_server_config(temp.path(), "Live BC Contract")?;
    if server.authentication != AuthMethod::AAD
        || server.tenant.as_deref() != Some("demo.onmicrosoft.com")
        || server.environment_name.as_deref() != Some("CDX Sandbox")
    {
        return Err(failure(format!(
            "repository fixture launch template parsed incorrectly: {server:?}"
        ))
        .into());
    }

    // The API page has SourceTable Customer, which needs the base app symbols
    // that the live run downloads first. This offline build checks the rest.
    let api_page = temp.path().join(SNAPPOINT_FILE);
    if !std::fs::read_to_string(&api_page)?.contains("LIVE_BC_SNAPPOINT") {
        return Err(failure("repository live fixture API page has no snappoint marker").into());
    }
    std::fs::remove_file(&api_page)?;
    let build = al_compile::native_compile(temp.path());
    if !build.success {
        return Err(failure(format!(
            "repository live fixture did not build:\n{}\n{:#?}",
            build.output, build.diagnostics
        ))
        .into());
    }
    let app_path = build
        .app_path
        .ok_or_else(|| failure("repository live fixture build produced no .app"))?;
    if !app_path.is_file() {
        return Err(failure(format!(
            "repository live fixture artifact is missing: {}",
            app_path.display()
        ))
        .into());
    }

    let source_path = temp.path().join("src/LiveContractTests.Codeunit.al");
    let source = std::fs::read_to_string(&source_path)?;
    if source.matches("LIVE_BC_BREAKPOINT").count() != 1 {
        return Err(failure("repository live fixture must have one breakpoint marker").into());
    }
    let workspace = al_workspace::Workspace::new();
    workspace.file_index.add_file(source_path, source);
    let classified = al_test::router::classify_all(&workspace)?;
    let exact = classified
        .iter()
        .find(|entry| {
            entry.codeunit_id == 98100
                && entry.codeunit_name == "Live Contract Tests"
                && entry.method_name == "PublishDebugAndSnapshot"
        })
        .ok_or_else(|| failure("repository live fixture exact test was not discovered"))?;
    if exact.decision != al_test::router::RoutingDecision::LiveBc {
        return Err(failure(format!(
            "repository live fixture routed as {:?}: {:?}",
            exact.decision, exact.reasons
        ))
        .into());
    }
    Ok(())
}

#[test]
fn repository_live_fixture_preflight_needs_no_external_project() -> AnyResult<()> {
    let output = generated_fixture_preflight("demo.onmicrosoft.com")?;
    if !output.status.success() {
        return Err(failure(format!(
            "generated fixture preflight failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
        .into());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.contains("Live BC contract preflight passed") {
        return Err(failure(format!("unexpected preflight output: {stdout}")).into());
    }
    if stdout.contains("fixture-preflight-token")
        || String::from_utf8_lossy(&output.stderr).contains("fixture-preflight-token")
    {
        return Err(failure("live preflight printed its bearer token").into());
    }
    Ok(())
}

#[test]
fn repository_live_fixture_preflight_does_not_require_gnu_sed_in_place() -> AnyResult<()> {
    let temp = tempfile::tempdir()?;
    let bash_env = temp.path().join("reject-sed-in-place.sh");
    std::fs::write(
        &bash_env,
        r#"sed() {
    case "${1:-}" in
        -i|-i*) echo "GNU-only sed -i rejected by contract test" >&2; return 97 ;;
    esac
    command sed "$@"
}
"#,
    )?;

    let output =
        generated_fixture_preflight_with_bash_env("demo.onmicrosoft.com", Some(&bash_env))?;
    if !output.status.success() {
        return Err(failure(format!(
            "generated fixture preflight requires GNU sed behavior:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
        .into());
    }
    Ok(())
}

#[test]
fn repository_live_fixture_rejects_an_unsafe_tenant_before_network() -> AnyResult<()> {
    let output = generated_fixture_preflight("https://example.com/other")?;
    if output.status.code() != Some(2) {
        return Err(failure(format!(
            "unsafe tenant preflight returned {:?}, expected 2",
            output.status.code()
        ))
        .into());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.contains("AL_LIVE_BC_TENANT must be a tenant GUID or domain") {
        return Err(failure(format!("unsafe tenant diagnostic was unclear: {stderr}")).into());
    }
    Ok(())
}

/// What one contract step reports.
enum Outcome {
    Pass(String),
    Skip(String),
}

struct StepReport {
    name: &'static str,
    api: &'static str,
    result: Result<Outcome, String>,
}

impl StepReport {
    fn status(&self) -> &'static str {
        match &self.result {
            Ok(Outcome::Pass(_)) => "PASS",
            Ok(Outcome::Skip(_)) => "SKIP",
            Err(_) => "FAIL",
        }
    }

    fn detail(&self) -> String {
        match &self.result {
            Ok(Outcome::Pass(detail)) | Ok(Outcome::Skip(detail)) => detail.clone(),
            Err(message) => message.clone(),
        }
    }
}

fn pass(detail: impl Into<String>) -> AnyResult<Outcome> {
    Ok(Outcome::Pass(detail.into()))
}

/// The text on one line, at most 700 characters.
fn first_line(text: &str) -> String {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    joined.chars().take(1600).collect()
}

/// Where the fixture app's API page sets its snappoint.
const SNAPPOINT_FILE: &str = "src/LiveContractApi.Page.al";

/// Business Central API root for the selected Sandbox, used only to create
/// server activity for the snapshot and profiling steps.
fn api_base(contract: &LiveContract) -> AnyResult<String> {
    let environment =
        contract.server.environment_name.clone().ok_or_else(|| {
            failure("UNAVAILABLE: the launch configuration has no environmentName")
        })?;
    Ok(format!(
        "https://api.businesscentral.dynamics.com/v2.0/{}/{}/api",
        contract.tenant()?,
        environment
    ))
}

struct Activity {
    requests: u32,
    failures: u32,
    last_status: u16,
}

/// Standard API GETs on the Sandbox. Each one is a WebServiceClient session
/// on the server, which is what the snapshot and profiling steps arm for.
/// `heavy` adds the large reads (general ledger entries, items) that give a
/// sampling profile something to sample.
async fn generate_activity(
    contract: &LiveContract,
    duration: Duration,
    heavy: bool,
) -> AnyResult<Activity> {
    let token = al_bc::http_auth::access_token_from_env()?
        .ok_or_else(|| failure("UNAVAILABLE: BC_ACCESS_TOKEN or BC_TOKEN is required"))?;
    let base = api_base(contract)?;
    let client = reqwest::Client::new();
    let get = |url: String| {
        let client = client.clone();
        let token = token.clone();
        async move {
            client
                .get(url)
                .bearer_auth(token)
                .header("Accept", "application/json")
                .send()
                .await
        }
    };
    let companies = get(format!("{base}/v2.0/companies")).await?;
    if !companies.status().is_success() {
        return Err(failure(format!(
            "GET /api/v2.0/companies answered {}",
            companies.status()
        ))
        .into());
    }
    let body: Value = companies.json().await?;
    let company = body["value"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["name"] == "CRONUS UK Ltd.")
                .or_else(|| rows.first())
        })
        .and_then(|row| row["id"].as_str())
        .ok_or_else(|| failure("the Sandbox lists no company"))?
        .to_string();
    let mut urls = vec![
        // The fixture's API page runs AL code that a snappoint and a sampling
        // profile can see.
        format!("{base}/alzed/contract/v1.0/companies({company})/liveCustomers"),
        format!("{base}/v2.0/companies({company})/customers"),
        format!("{base}/v2.0/companies({company})/salesInvoices"),
    ];
    if heavy {
        urls.push(format!(
            "{base}/v2.0/companies({company})/generalLedgerEntries?$top=5000"
        ));
        urls.push(format!("{base}/v2.0/companies({company})/items?$top=2000"));
    }
    let mut activity = Activity {
        requests: 1,
        failures: 0,
        last_status: 200,
    };
    let deadline = std::time::Instant::now() + duration;
    loop {
        for url in &urls {
            let response = get(url.clone()).await?;
            activity.requests += 1;
            activity.last_status = response.status().as_u16();
            if !response.status().is_success() {
                activity.failures += 1;
            }
            // Read the body so the server finishes the request.
            let _ = response.bytes().await?;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
    }
    Ok(activity)
}

/// publish (POST dev/apps)
async fn step_publish(contract: &LiveContract) -> AnyResult<Outcome> {
    publish_succeeds_on_http_status(&contract.project, &contract.config_name).await?;
    pass("uploaded and installed through dev/apps")
}

/// DAP attach, breakpoint, stepping and evaluate (Debugger hub, dev/debug)
async fn step_dap(contract: &LiveContract) -> AnyResult<Outcome> {
    exercise_native_dap(contract).await?;
    pass("breakpoint verified, hit, locals and evaluate answered")
}

/// live test run (TestRunnerHub). The client does not exist (finding
/// testing-01, Docs/testing-guide.md), so the step is reported SKIP and never
/// counted as a pass. `AL_LIVE_BC_TRY_TEST_RUN=1` runs the old CLI route.
fn step_test_run(contract: &LiveContract) -> AnyResult<Outcome> {
    if std::env::var("AL_LIVE_BC_TRY_TEST_RUN").as_deref() != Ok("1") {
        return Ok(Outcome::Skip(
            "testing-01: no TestRunnerHub client, so a live test run is not attempted".to_string(),
        ));
    }
    exercise_cli_live_test_and_snapshots(contract)?;
    pass("test-run and test-snapshot capture, validate, replay and diff")
}

/// The 1-based line of `marker` in a project file.
fn marker_line(project: &Path, file: &str, marker: &str) -> AnyResult<usize> {
    let text = std::fs::read_to_string(project.join(file))
        .map_err(|error| failure(format!("read {file}: {error}")))?;
    text.lines()
        .position(|line| line.contains(marker))
        .map(|index| index + 1)
        .ok_or_else(|| failure(format!("{file} has no {marker} marker")).into())
}

/// snapshot debugging (snapshotdebugger attach, status, finish, then show)
async fn step_snapshot(contract: &LiveContract) -> AnyResult<Outcome> {
    let _daemon = DaemonGuard::reset(&contract.project);
    let line = marker_line(&contract.project, SNAPPOINT_FILE, "LIVE_BC_SNAPPOINT")?;
    let snap = format!("{SNAPPOINT_FILE}:{line}");
    let started = cli_json(
        contract,
        &[
            "snapshot",
            "start",
            "--break-on-next",
            "WebServiceClient",
            "--snap",
            &snap,
            "--config",
            &contract.config_name,
        ],
    )?;
    let context = started["debuggingContext"]
        .as_str()
        .ok_or_else(|| {
            failure(format!(
                "snapshot start returned no debuggingContext: {started}"
            ))
        })?
        .to_string();
    let before = cli_json(contract, &["snapshot", "status", "--context", &context]);
    let activity = generate_activity(contract, Duration::from_secs(5), false).await;
    // The server forgets a session that has run, so a status after the
    // activity may answer 404. It is reported, not required.
    let after = cli_json(contract, &["snapshot", "status", "--context", &context]);
    let finished = cli_json(contract, &["snapshot", "finish", "--context", &context]);
    let before = before?;
    let activity = activity?;
    if activity.failures > 0 {
        return Err(failure(format!(
            "API activity had {} failed requests, last status {}",
            activity.failures, activity.last_status
        ))
        .into());
    }
    let after_text = match &after {
        Ok(status) => first_line(status["status"].as_str().unwrap_or("no status")),
        Err(error) => first_line(&error.to_string()),
    };
    let finished = finished.map_err(|error| {
        failure(format!(
            "finish failed. Status before activity {}, after activity {after_text}. {error}",
            before["status"]
        ))
    })?;
    match finished["zipPath"].as_str() {
        Some(zip) if Path::new(zip).is_file() => {
            let shown = cli_json(contract, &["snapshot", "show", zip])?;
            let stops = shown["stops"].as_array().map_or(0, Vec::len);
            if stops == 0 {
                return Err(failure(format!("snapshot zip has no stops: {shown}")).into());
            }
            pass(format!("snapshot zip saved, show lists {stops} stops"))
        }
        _ => Err(failure(format!(
            "finish returned no snapshot for a snappoint at {snap}: {}. Status after activity: {after_text}",
            first_line(finished["message"].as_str().unwrap_or("no message"))
        ))
        .into()),
    }
}

/// profiling (snapshotdebugger attach with ExecutionContext Profiling, sampling)
async fn step_profile(contract: &LiveContract) -> AnyResult<Outcome> {
    let _daemon = DaemonGuard::reset(&contract.project);
    let started = cli_json(
        contract,
        &[
            "profile",
            "start",
            "--break-on-next",
            "WebServiceClient",
            "--config",
            &contract.config_name,
        ],
    )?;
    let context = started["debuggingContext"]
        .as_str()
        .ok_or_else(|| {
            failure(format!(
                "profile start returned no debuggingContext: {started}"
            ))
        })?
        .to_string();
    let seconds = std::env::var("AL_LIVE_BC_PROFILE_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(60);
    let activity = generate_activity(contract, Duration::from_secs(seconds), true).await;
    let status = cli_json(contract, &["profile", "status", "--context", &context]);
    let stopped = cli_json(contract, &["profile", "stop", "--context", &context]);
    let activity = activity?;
    status?;
    let stopped = stopped?;
    let load = format!(
        "{} API requests over {seconds} s, {} failed",
        activity.requests, activity.failures
    );
    match stopped["profilePath"].as_str() {
        Some(path) if Path::new(path).is_file() => {
            let analysis = cli_json(contract, &["profile", "analyze", path])?;
            let nodes = std::fs::read_to_string(path)
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                .map_or(0, |profile| {
                    profile["nodes"].as_array().map_or(0, Vec::len)
                        + profile["samples"].as_array().map_or(0, Vec::len)
                });
            if nodes == 0 {
                let head = std::fs::read_to_string(path).unwrap_or_default();
                return Err(failure(format!(
                    "profile file holds no nodes or samples after {load}. File: {}. Analyze: {analysis}",
                    first_line(&head.chars().take(300).collect::<String>())
                ))
                .into());
            }
            pass(format!(
                "profile with {nodes} nodes and samples after {load}"
            ))
        }
        _ => Err(failure(format!(
            "no profile came back after {load}: {}",
            first_line(&stopped.to_string())
        ))
        .into()),
    }
}

/// symbol download (server route, dev/packages)
fn step_symbols(contract: &LiveContract) -> AnyResult<Outcome> {
    let result = cli_json(contract, &["download-symbols", "--source", "server"])?;
    let downloaded = result["downloaded"].as_u64().unwrap_or(0);
    let failed = result["failed"].as_u64().unwrap_or(0);
    if downloaded == 0 || failed > 0 {
        return Err(failure(format!(
            "download-symbols --source server: {downloaded} downloaded, {failed} failed: {result}"
        ))
        .into());
    }
    let packages = std::fs::read_dir(contract.project.join(".alpackages"))?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "app"))
        .count();
    if packages == 0 {
        return Err(failure("download-symbols left no .app in .alpackages").into());
    }
    pass(format!("{downloaded} packages downloaded"))
}

fn print_summary(reports: &[StepReport]) {
    println!("\n=== Live BC contract summary ===");
    println!("{:<20} {:<6} {:<46} detail", "step", "result", "BC API");
    for report in reports {
        println!(
            "{:<20} {:<6} {:<46} {}",
            report.name,
            report.status(),
            report.api,
            first_line(&report.detail())
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires explicit live Business Central inputs; run via make live-bc-contracts"]
async fn live_bc_publish_dap_test_and_snapshot_contract() -> AnyResult<()> {
    let contract = LiveContract::from_env()?;
    // Every step runs. One failing step does not hide the others, and the test
    // fails at the end if any step failed. SKIP is not a pass.
    let mut reports = Vec::new();
    macro_rules! step {
        ($name:expr, $api:expr, $run:expr) => {{
            println!("--- step {} ---", $name);
            let result = $run.await.map_err(|error| error.to_string());
            reports.push(StepReport {
                name: $name,
                api: $api,
                result,
            });
        }};
    }
    // The publish compiles the fixture against .alpackages, so the symbol
    // download runs first. Its row is moved to the end of the table.
    step!(
        "symbol download",
        "GET /dev/packages (server route)",
        async { step_symbols(&contract) }
    );
    let symbols = reports.remove(0);
    step!("publish", "POST /dev/apps", step_publish(&contract));
    step!(
        "dap attach",
        "debugger hub, dev/debug breakpoints",
        step_dap(&contract)
    );
    step!(
        "live test run",
        "TestRunnerHub (no client, testing-01)",
        async { step_test_run(&contract) }
    );
    step!(
        "snapshot debugging",
        "v2.0/{env}/snapshotdebugger/attach|status|finish",
        step_snapshot(&contract)
    );
    step!(
        "profiling",
        "snapshotdebugger attach, ExecutionContext Profiling",
        step_profile(&contract)
    );
    reports.push(symbols);
    print_summary(&reports);
    let failed: Vec<&str> = reports
        .iter()
        .filter(|report| report.status() == "FAIL")
        .map(|report| report.name)
        .collect();
    if !failed.is_empty() {
        return Err(failure(format!("live contract steps failed: {}", failed.join(", "))).into());
    }
    Ok(())
}
