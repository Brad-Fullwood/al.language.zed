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

fn cli_json(project: &Path, args: &[&str]) -> AnyResult<Value> {
    let output = Command::new(al_explorer_binary())
        .arg("--json")
        .args(args)
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

async fn publish_with_completed_status(project: &Path, config_name: &str) -> AnyResult<()> {
    let workspace = al_workspace::Workspace::new();
    *workspace.config.write().await = al_project::config::AlConfig::load_effective(project)?;
    al_workspace::initialize_core_workspace(&workspace, project).await?;
    let mut config = al_publish::PublishConfig::new(project);
    config.config_name = Some(config_name.to_string());
    let result = al_publish::publish(&workspace, &config).await?;
    if !result.success {
        return Err(failure(format!(
            "publish/install did not reach completed state: {}",
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

    let token = al_bc::http_auth::access_token_from_env()?
        .ok_or_else(|| failure("UNAVAILABLE: no BC bearer token"))?;
    let runner = TestRunnerClient::with_access_token(&contract.server, Some(&token))?;
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
        &contract.project,
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
        &contract.project,
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
    let validated = cli_json(
        &contract.project,
        &["test-snapshot", "validate", &snapshot_string],
    )?;
    if validated["sampleCount"].as_u64().unwrap_or(0) == 0 {
        return Err(failure(format!(
            "captured snapshot did not validate with samples: {validated}"
        ))
        .into());
    }
    let replay = cli_json(
        &contract.project,
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
        &contract.project,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires explicit live Business Central inputs; run via make live-bc-contracts"]
async fn live_bc_publish_dap_test_and_snapshot_contract() -> AnyResult<()> {
    let contract = LiveContract::from_env()?;
    publish_with_completed_status(&contract.project, &contract.config_name).await?;
    exercise_native_dap(&contract).await?;
    exercise_cli_live_test_and_snapshots(&contract)?;
    Ok(())
}
