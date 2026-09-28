//! Live Business Central test snapshot capture.
//!
//! This backend deliberately composes the existing BC test runner and native
//! debug hub instead of pretending a local interpreter sample is equivalent.
//! The caller supplies resolved workspace breakpoint metadata and is
//! responsible for persisting the returned `al_snapshot::Snapshot`.
//!
//! The capture logic reaches the debug hub through the private
//! `SnapshotDebugger` trait and the test runner through `SnapshotTestRunner`.
//! `NativeDebugSession` and `TestRunnerClient` implement them for a live
//! server. The unit tests implement them with scripted fakes, so the capture
//! logic runs without a server.

use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use al_bc::launch::BcServerConfig;
use al_dap::dap::bc_debug::BcDebugConfig;
use al_dap::dap::types::{BreakpointInfo, DebugState, SessionStatus};
use al_dap::native_debug::NativeDebugSession;
use al_snapshot::{Sample, Snapshot};
use thiserror::Error;

use crate::error::TestRunnerError;
use crate::result::TestCodeunitResult;
use crate::test_runner::TestRunnerClient;

#[derive(Debug, Clone)]
pub struct SnapshotBreakpoint {
    pub file: String,
    pub line: u32,
    pub object_type: i32,
    pub object_id: i32,
    pub condition: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LiveSnapshotRequest {
    pub server: BcServerConfig,
    pub debug: BcDebugConfig,
    pub access_token: String,
    pub codeunit_id: i32,
    pub codeunit_name: String,
    pub method_name: String,
    pub bc_version: String,
    pub source_hash: String,
    pub breakpoints: Vec<SnapshotBreakpoint>,
    pub timeout: Duration,
}

#[derive(Debug, Error)]
pub enum SnapshotCaptureError {
    #[error("snapshot capture requires at least one breakpoint")]
    MissingBreakpoints,
    #[error("debug session failed: {0}")]
    Debug(#[from] al_dap::dap::DapError),
    #[error("test runner failed: {0}")]
    Test(#[from] crate::error::TestRunnerError),
    #[error("Business Central rejected breakpoint {file}:{line}")]
    BreakpointRejected { file: String, line: u32 },
    #[error(
        "Business Central returned {returned} breakpoints for {file}, but {expected} were requested"
    )]
    BreakpointCountMismatch {
        file: String,
        expected: usize,
        returned: usize,
    },
    #[error("Business Central returned breakpoint ID {0}, which is outside the snapshot format")]
    InvalidBreakpointId(i64),
    #[error("verified breakpoint {file}:{line} lost its server ID")]
    MissingBreakpointId { file: String, line: u32 },
    #[error("snapshot capture timed out after {0} ms")]
    Timeout(u128),
    #[error("debug session stopped at an unconfigured location (line {line}, object {object:?})")]
    UnexpectedStop {
        line: u32,
        object: Option<(i32, i32)>,
    },
    #[error(
        "debug session stopped on line {line} and BC did not report which object, so the stop \
         matches {count} configured breakpoints: {files:?}"
    )]
    AmbiguousStop {
        line: u32,
        count: usize,
        files: Vec<String>,
    },
    #[error("debug session paused without a source location (object {object:?})")]
    MissingStopLocation { object: Option<(i32, i32)> },
    #[error("live test completed without hitting any configured breakpoint")]
    NoSamples,
    #[error(
        "live test runner did not execute exactly requested method '{expected}' (returned: {returned:?})"
    )]
    UnexpectedTestResult {
        expected: String,
        returned: Vec<String>,
    },
    #[error("captured variables could not be serialized: {0}")]
    Variables(#[from] serde_json::Error),
    #[error("snapshot clock is earlier than the Unix epoch: {0}")]
    Clock(#[from] std::time::SystemTimeError),
    #[error(
        "snapshot capture failed ({capture}); debug-session shutdown also failed ({shutdown})"
    )]
    CaptureAndShutdown { capture: String, shutdown: String },
}

/// The debug hub calls a snapshot capture makes.
trait SnapshotDebugger {
    async fn set_breakpoints(
        &mut self,
        file: &str,
        lines: &[(u32, Option<&str>)],
        object_type: i32,
        object_id: i32,
    ) -> al_dap::dap::Result<Vec<BreakpointInfo>>;

    /// The session state, and the (object type, object ID) of the most recent
    /// stop when BC reported one.
    async fn state_and_object(&mut self) -> al_dap::dap::Result<(DebugState, Option<(i32, i32)>)>;

    async fn continue_exec(&mut self) -> al_dap::dap::Result<()>;

    async fn stop(&mut self) -> al_dap::dap::Result<()>;
}

impl SnapshotDebugger for NativeDebugSession {
    async fn set_breakpoints(
        &mut self,
        file: &str,
        lines: &[(u32, Option<&str>)],
        object_type: i32,
        object_id: i32,
    ) -> al_dap::dap::Result<Vec<BreakpointInfo>> {
        NativeDebugSession::set_breakpoints(self, file, lines, object_type, object_id).await
    }

    async fn state_and_object(&mut self) -> al_dap::dap::Result<(DebugState, Option<(i32, i32)>)> {
        let state = NativeDebugSession::state(self).await?;
        Ok((state, NativeDebugSession::current_object(self)))
    }

    async fn continue_exec(&mut self) -> al_dap::dap::Result<()> {
        NativeDebugSession::continue_exec(self).await.map(drop)
    }

    async fn stop(&mut self) -> al_dap::dap::Result<()> {
        NativeDebugSession::stop(self).await
    }
}

/// The test runner call a snapshot capture makes.
trait SnapshotTestRunner {
    async fn run_codeunit(
        &self,
        codeunit_id: i32,
        codeunit_name: &str,
        method: Option<&str>,
    ) -> Result<TestCodeunitResult, TestRunnerError>;
}

impl SnapshotTestRunner for TestRunnerClient {
    async fn run_codeunit(
        &self,
        codeunit_id: i32,
        codeunit_name: &str,
        method: Option<&str>,
    ) -> Result<TestCodeunitResult, TestRunnerError> {
        TestRunnerClient::run_codeunit(self, codeunit_id, codeunit_name, method).await
    }
}

pub async fn capture_live_snapshot(
    request: LiveSnapshotRequest,
) -> Result<(Snapshot, TestCodeunitResult), SnapshotCaptureError> {
    capture(
        &request,
        NativeDebugSession::start(request.debug.clone(), &request.access_token),
        || TestRunnerClient::with_access_token(&request.server, Some(&request.access_token)),
    )
    .await
}

/// Run one capture. `start_debugger` is awaited only when the request has
/// breakpoints. `connect_runner` is called once the breakpoints are set.
async fn capture<D, R>(
    request: &LiveSnapshotRequest,
    start_debugger: impl Future<Output = al_dap::dap::Result<D>>,
    connect_runner: impl FnOnce() -> Result<R, TestRunnerError>,
) -> Result<(Snapshot, TestCodeunitResult), SnapshotCaptureError>
where
    D: SnapshotDebugger,
    R: SnapshotTestRunner,
{
    if request.breakpoints.is_empty() {
        return Err(SnapshotCaptureError::MissingBreakpoints);
    }

    let mut debug = start_debugger.await?;
    let capture_result = capture_with_session(request, &mut debug, connect_runner).await;
    let shutdown_result = debug.stop().await;
    match (capture_result, shutdown_result) {
        (Ok(snapshot), Ok(())) => Ok(snapshot),
        // The snapshot is complete and useful; a debugger BC failed to detach
        // is an operational problem for the next session, not a reason to
        // throw the capture away.
        (Ok(snapshot), Err(shutdown)) => {
            tracing::warn!(
                %shutdown,
                "snapshot captured, but the BC debug session did not detach"
            );
            Ok(snapshot)
        }
        (Err(capture), Ok(())) => Err(capture),
        (Err(capture), Err(shutdown)) => Err(SnapshotCaptureError::CaptureAndShutdown {
            capture: capture.to_string(),
            shutdown: shutdown.to_string(),
        }),
    }
}

async fn capture_with_session<D, R>(
    request: &LiveSnapshotRequest,
    debug: &mut D,
    connect_runner: impl FnOnce() -> Result<R, TestRunnerError>,
) -> Result<(Snapshot, TestCodeunitResult), SnapshotCaptureError>
where
    D: SnapshotDebugger,
    R: SnapshotTestRunner,
{
    let mut grouped =
        std::collections::BTreeMap::<(String, i32, i32), Vec<(u32, Option<String>)>>::new();
    for breakpoint in &request.breakpoints {
        grouped
            .entry((
                breakpoint.file.clone(),
                breakpoint.object_type,
                breakpoint.object_id,
            ))
            .or_default()
            .push((breakpoint.line, breakpoint.condition.clone()));
    }
    let mut breakpoint_ids = HashMap::<(i32, i32, u32), u32>::new();
    for ((file, object_type, object_id), points) in grouped {
        let lines = points
            .iter()
            .map(|(line, condition)| (*line, condition.as_deref()))
            .collect::<Vec<_>>();
        let infos = debug
            .set_breakpoints(&file, &lines, object_type, object_id)
            .await?;
        if infos.len() != points.len() {
            return Err(SnapshotCaptureError::BreakpointCountMismatch {
                file: file.clone(),
                expected: points.len(),
                returned: infos.len(),
            });
        }
        for ((line, _), info) in points.iter().zip(infos.iter()) {
            if !info.verified || info.id <= 0 {
                return Err(SnapshotCaptureError::BreakpointRejected {
                    file: file.clone(),
                    line: *line,
                });
            }
            let breakpoint_id = u32::try_from(info.id)
                .map_err(|_| SnapshotCaptureError::InvalidBreakpointId(info.id))?;
            breakpoint_ids.insert((object_type, object_id, *line), breakpoint_id);
        }
    }

    let client = connect_runner()?;
    let test_run = client.run_codeunit(
        request.codeunit_id,
        &request.codeunit_name,
        Some(&request.method_name),
    );
    tokio::pin!(test_run);
    let mut interval = tokio::time::interval(Duration::from_millis(20));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let deadline = tokio::time::sleep(request.timeout);
    tokio::pin!(deadline);
    let mut samples = Vec::new();
    let mut iterations = HashMap::<u32, u32>::new();

    let test_result = loop {
        tokio::select! {
            result = &mut test_run => break result?,
            _ = &mut deadline => {
                return Err(SnapshotCaptureError::Timeout(request.timeout.as_millis()));
            }
            _ = interval.tick() => {
                let (state, current_object) = debug.state_and_object().await?;
                if state.status != SessionStatus::Paused {
                    continue;
                }
                let location = state
                    .location
                    .ok_or(SnapshotCaptureError::MissingStopLocation {
                        object: current_object,
                    })?;
                // BC does not always report the object it stopped in, and the
                // Break event carries no source path either, so narrow by
                // whatever the stop does identify.
                let candidates = request
                    .breakpoints
                    .iter()
                    .filter(|breakpoint| {
                        breakpoint.line == location.line
                            && current_object.is_none_or(|(object_type, object_id)| {
                                breakpoint.object_type == object_type
                                    && breakpoint.object_id == object_id
                            })
                            && (location.file.is_empty() || breakpoint.file == location.file)
                    })
                    .collect::<Vec<_>>();
                let configured = match candidates.as_slice() {
                    [breakpoint] => *breakpoint,
                    // Two breakpoints on the same line of different files and
                    // no object identity: the capture was configured exactly as
                    // asked, and saying the stop was "unconfigured" sends the
                    // reader after the wrong thing.
                    [_, _, ..] => {
                        return Err(SnapshotCaptureError::AmbiguousStop {
                            line: location.line,
                            count: candidates.len(),
                            files: candidates
                                .iter()
                                .map(|breakpoint| breakpoint.file.clone())
                                .collect(),
                        });
                    }
                    [] => {
                        return Err(SnapshotCaptureError::UnexpectedStop {
                            line: location.line,
                            object: current_object,
                        });
                    }
                };
                let breakpoint_id = breakpoint_ids
                    .get(&(
                        configured.object_type,
                        configured.object_id,
                        configured.line,
                    ))
                    .copied()
                    .ok_or_else(|| SnapshotCaptureError::MissingBreakpointId {
                        file: configured.file.clone(),
                        line: configured.line,
                    })?;
                let iteration = iterations.entry(breakpoint_id).or_default();
                samples.push(Sample {
                    breakpoint_id,
                    file: configured.file.clone(),
                    line: configured.line,
                    condition: configured.condition.clone(),
                    iteration: *iteration,
                    variables: serde_json::to_value(state.variables)?,
                });
                *iteration += 1;
                debug.continue_exec().await?;
            }
        }
    };

    if samples.is_empty() {
        return Err(SnapshotCaptureError::NoSamples);
    }
    let returned_methods = test_result
        .methods
        .iter()
        .map(|method| method.name.clone())
        .collect::<Vec<_>>();
    if returned_methods.len() != 1
        || !returned_methods[0].eq_ignore_ascii_case(&request.method_name)
    {
        return Err(SnapshotCaptureError::UnexpectedTestResult {
            expected: request.method_name.clone(),
            returned: returned_methods,
        });
    }

    let captured = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?;
    let captured_at = captured.as_secs();
    let run_id = format!(
        "{}-{}-{}",
        request.codeunit_id,
        request.method_name,
        captured.as_nanos()
    );
    Ok((
        Snapshot {
            run_id,
            codeunit_id: request.codeunit_id,
            method_name: request.method_name.clone(),
            bc_version: request.bc_version.clone(),
            source_hash: request.source_hash.clone(),
            captured_at,
            samples,
        },
        test_result,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_bc::launch::{AuthMethod, EnvironmentType};
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::rc::Rc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use al_dap::dap::types::{Location, Variable};
    use al_dap::dap::DapError;
    use tokio::sync::Notify;

    use crate::result::{TestMethodResult, TestStatus};

    #[tokio::test]
    async fn empty_breakpoint_capture_fails_before_connecting() {
        let request = LiveSnapshotRequest {
            server: BcServerConfig {
                name: "unit".to_string(),
                environment_type: EnvironmentType::OnPrem,
                server: Some("http://127.0.0.1:1".to_string()),
                server_instance: Some("BC".to_string()),
                port: None,
                environment_name: None,
                tenant: Some("default".to_string()),
                authentication: AuthMethod::UserPassword,
                accept_invalid_certs: false,
                debug_args: serde_json::json!({}),
            },
            debug: BcDebugConfig::from_dap_args(&serde_json::json!({})),
            access_token: String::new(),
            codeunit_id: 50100,
            codeunit_name: "Snapshot Tests".to_string(),
            method_name: "Captures".to_string(),
            bc_version: "26.0.0.0".to_string(),
            source_hash: "abc".to_string(),
            breakpoints: Vec::new(),
            timeout: Duration::from_secs(1),
        };
        let error = capture_live_snapshot(request)
            .await
            .expect_err("empty breakpoints must fail");
        assert!(matches!(error, SnapshotCaptureError::MissingBreakpoints));
    }

    /// A call the capture made on the fake debugger or the fake runner.
    #[derive(Debug, PartialEq)]
    enum Call {
        SetBreakpoints {
            file: String,
            lines: Vec<(u32, Option<String>)>,
            object: (i32, i32),
        },
        ConnectRunner,
        RunCodeunit {
            codeunit_id: i32,
            codeunit_name: String,
            method: Option<String>,
        },
        Continue,
        Stop,
    }

    type CallLog = Rc<RefCell<Vec<Call>>>;

    type CaptureResult = Result<(Snapshot, TestCodeunitResult), SnapshotCaptureError>;

    /// A debug hub that answers from a script and records each call.
    struct FakeDebugger {
        /// Replies to `set_breakpoints` by file. A file without an entry has
        /// every line verified with the ID `1000 + line`.
        replies: HashMap<String, Vec<BreakpointInfo>>,
        /// What `state_and_object` returns, one entry per call. Once they are
        /// used up the session reports `Running` and `polls_done` is notified.
        polls: VecDeque<(DebugState, Option<(i32, i32)>)>,
        polls_done: Rc<Notify>,
        stop_error: Option<DapError>,
        calls: CallLog,
    }

    impl SnapshotDebugger for FakeDebugger {
        async fn set_breakpoints(
            &mut self,
            file: &str,
            lines: &[(u32, Option<&str>)],
            object_type: i32,
            object_id: i32,
        ) -> al_dap::dap::Result<Vec<BreakpointInfo>> {
            self.calls.borrow_mut().push(Call::SetBreakpoints {
                file: file.to_string(),
                lines: lines
                    .iter()
                    .map(|(line, condition)| (*line, condition.map(str::to_string)))
                    .collect(),
                object: (object_type, object_id),
            });
            Ok(self.replies.remove(file).unwrap_or_else(|| {
                lines
                    .iter()
                    .map(|(line, _)| verified(file, *line, 1000 + i64::from(*line)))
                    .collect()
            }))
        }

        async fn state_and_object(
            &mut self,
        ) -> al_dap::dap::Result<(DebugState, Option<(i32, i32)>)> {
            let next = self.polls.pop_front();
            if self.polls.is_empty() {
                self.polls_done.notify_one();
            }
            Ok(next.unwrap_or_else(|| (state(SessionStatus::Running, None), None)))
        }

        async fn continue_exec(&mut self) -> al_dap::dap::Result<()> {
            self.calls.borrow_mut().push(Call::Continue);
            Ok(())
        }

        async fn stop(&mut self) -> al_dap::dap::Result<()> {
            self.calls.borrow_mut().push(Call::Stop);
            self.stop_error.take().map_or(Ok(()), Err)
        }
    }

    /// A test runner whose run finishes once the debugger script is used up.
    struct FakeRunner {
        /// What the run returns. `None` keeps the run going until the
        /// capture gives up.
        outcome: RefCell<Option<Result<TestCodeunitResult, TestRunnerError>>>,
        polls_done: Rc<Notify>,
        calls: CallLog,
    }

    impl SnapshotTestRunner for FakeRunner {
        fn run_codeunit(
            &self,
            codeunit_id: i32,
            codeunit_name: &str,
            method: Option<&str>,
        ) -> impl Future<Output = Result<TestCodeunitResult, TestRunnerError>> {
            // Recorded here rather than in the future, so the call lands in
            // the log when the capture makes it.
            self.calls.borrow_mut().push(Call::RunCodeunit {
                codeunit_id,
                codeunit_name: codeunit_name.to_string(),
                method: method.map(str::to_string),
            });
            let outcome = self.outcome.borrow_mut().take();
            let polls_done = Rc::clone(&self.polls_done);
            async move {
                let Some(outcome) = outcome else {
                    return std::future::pending().await;
                };
                polls_done.notified().await;
                outcome
            }
        }
    }

    /// What the fakes do during one capture.
    struct Script {
        replies: HashMap<String, Vec<BreakpointInfo>>,
        polls: Vec<(DebugState, Option<(i32, i32)>)>,
        connect_error: Option<TestRunnerError>,
        outcome: Option<Result<TestCodeunitResult, TestRunnerError>>,
        stop_error: Option<DapError>,
    }

    /// Every breakpoint is verified, the session never pauses, and the run
    /// passes `Captures`.
    fn script() -> Script {
        Script {
            replies: HashMap::new(),
            polls: Vec::new(),
            connect_error: None,
            outcome: Some(Ok(result_for(&["Captures"]))),
            stop_error: None,
        }
    }

    impl Script {
        /// Run `capture` against the fakes, and return its result and the
        /// calls it made in order.
        async fn run(self, request: &LiveSnapshotRequest) -> (CaptureResult, Vec<Call>) {
            let calls = CallLog::default();
            let polls_done = Rc::new(Notify::new());
            let debugger = FakeDebugger {
                replies: self.replies,
                polls: self.polls.into(),
                polls_done: Rc::clone(&polls_done),
                stop_error: self.stop_error,
                calls: Rc::clone(&calls),
            };
            let runner = FakeRunner {
                outcome: RefCell::new(self.outcome),
                polls_done,
                calls: Rc::clone(&calls),
            };
            let connect_error = self.connect_error;
            let connect_calls = Rc::clone(&calls);
            let result = capture(request, async { Ok(debugger) }, move || {
                connect_calls.borrow_mut().push(Call::ConnectRunner);
                match connect_error {
                    Some(error) => Err(error),
                    None => Ok(runner),
                }
            })
            .await;
            (result, calls.take())
        }
    }

    /// A request for `Captures` in codeunit 50100 "Snapshot Tests" with a
    /// 60 second timeout.
    fn request(breakpoints: Vec<SnapshotBreakpoint>) -> LiveSnapshotRequest {
        LiveSnapshotRequest {
            server: BcServerConfig {
                name: "unit".to_string(),
                environment_type: EnvironmentType::OnPrem,
                server: Some("http://127.0.0.1:1".to_string()),
                server_instance: Some("BC".to_string()),
                port: None,
                environment_name: None,
                tenant: Some("default".to_string()),
                authentication: AuthMethod::UserPassword,
                accept_invalid_certs: false,
                debug_args: serde_json::json!({}),
            },
            debug: BcDebugConfig::from_dap_args(&serde_json::json!({})),
            access_token: String::new(),
            codeunit_id: 50100,
            codeunit_name: "Snapshot Tests".to_string(),
            method_name: "Captures".to_string(),
            bc_version: "26.0.0.0".to_string(),
            source_hash: "abc".to_string(),
            breakpoints,
            timeout: Duration::from_secs(60),
        }
    }

    /// An unconditional breakpoint in codeunit `object_id` (object type 5).
    fn breakpoint(file: &str, line: u32, object_id: i32) -> SnapshotBreakpoint {
        SnapshotBreakpoint {
            file: file.to_string(),
            line,
            object_type: 5,
            object_id,
            condition: None,
        }
    }

    fn verified(file: &str, line: u32, id: i64) -> BreakpointInfo {
        BreakpointInfo {
            id,
            file: file.to_string(),
            line,
            condition: None,
            verified: true,
        }
    }

    fn state(status: SessionStatus, location: Option<(&str, u32)>) -> DebugState {
        DebugState {
            status,
            session_id: "session".to_string(),
            location: location.map(|(file, line)| Location {
                file: file.to_string(),
                line,
                column: 1,
                procedure: None,
            }),
            stack: Vec::new(),
            variables: Vec::new(),
            thread_id: None,
        }
    }

    fn paused_at(file: &str, line: u32, variables: Vec<Variable>) -> DebugState {
        DebugState {
            variables,
            ..state(SessionStatus::Paused, Some((file, line)))
        }
    }

    fn integer(name: &str, value: &str) -> Variable {
        Variable {
            name: name.to_string(),
            value: value.to_string(),
            type_name: "Integer".to_string(),
            fields: Vec::new(),
        }
    }

    /// A result for codeunit 50100 in which every named method passed.
    fn result_for(methods: &[&str]) -> TestCodeunitResult {
        TestCodeunitResult::from_methods(
            "Snapshot Tests".to_string(),
            50100,
            methods
                .iter()
                .map(|name| TestMethodResult {
                    name: name.to_string(),
                    status: TestStatus::Pass,
                    error: None,
                    duration_ms: None,
                    failure_kind: None,
                })
                .collect(),
        )
    }

    /// A request with no breakpoints fails with `MissingBreakpoints` before
    /// the debug session starts or the runner connects.
    #[tokio::test]
    async fn no_breakpoints_fail_before_the_debugger_starts_or_the_runner_connects() {
        let started = Cell::new(false);
        let connected = Cell::new(false);
        let result = capture(
            &request(Vec::new()),
            async {
                started.set(true);
                Err::<FakeDebugger, _>(DapError::ConnectionFailed("unused".to_string()))
            },
            || {
                connected.set(true);
                Err::<FakeRunner, _>(TestRunnerError::MissingCredentials)
            },
        )
        .await;
        let error = result.expect_err("no breakpoints");
        assert!(
            matches!(error, SnapshotCaptureError::MissingBreakpoints),
            "got {error:?}"
        );
        assert!(!started.get(), "the debug session must not start");
        assert!(!connected.get(), "the runner must not connect");
    }

    /// Breakpoints on B.al line 5 (object 50101), A.al line 12 and A.al line
    /// 10 with a condition (both object 50100), and one stop on A.al line 12.
    /// The capture sets the breakpoints in one call per file and object, A.al
    /// before B.al, with the lines in request order. Then it connects the
    /// runner, runs `Captures` in codeunit 50100, continues after the stop and
    /// stops the session last.
    #[tokio::test]
    async fn breakpoints_are_set_per_file_and_object_before_the_test_runs() {
        let request = request(vec![
            breakpoint("B.al", 5, 50101),
            breakpoint("A.al", 12, 50100),
            SnapshotBreakpoint {
                condition: Some("Count > 1".to_string()),
                ..breakpoint("A.al", 10, 50100)
            },
        ]);
        let (result, calls) = Script {
            polls: vec![(paused_at("A.al", 12, Vec::new()), Some((5, 50100)))],
            ..script()
        }
        .run(&request)
        .await;
        result.expect("one stop at a configured breakpoint");
        assert_eq!(
            calls,
            vec![
                Call::SetBreakpoints {
                    file: "A.al".to_string(),
                    lines: vec![(12, None), (10, Some("Count > 1".to_string()))],
                    object: (5, 50100),
                },
                Call::SetBreakpoints {
                    file: "B.al".to_string(),
                    lines: vec![(5, None)],
                    object: (5, 50101),
                },
                Call::ConnectRunner,
                Call::RunCodeunit {
                    codeunit_id: 50100,
                    codeunit_name: "Snapshot Tests".to_string(),
                    method: Some("Captures".to_string()),
                },
                Call::Continue,
                Call::Stop,
            ]
        );
    }

    /// Breakpoints on A.al line 10 and line 12 (with a condition), and stops
    /// on line 10, line 12 and line 10 again with an Integer `Count` of 1, 2
    /// and 3. The fake gives each line the ID 1000 + line. The snapshot has
    /// three samples in stop order with IDs 1010, 1012 and 1010, the condition
    /// on the line 12 sample only, and iterations 0, 0 and 1, since the
    /// iteration counts earlier hits of the same breakpoint. The header comes
    /// from the request, `run_id` is `50100-Captures-` and the capture time in
    /// nanoseconds, `captured_at` is that time in whole seconds, and the run's
    /// result comes back beside the snapshot.
    #[tokio::test]
    async fn each_stop_becomes_a_sample_with_its_breakpoint_id_and_hit_count() {
        let request = request(vec![
            breakpoint("A.al", 10, 50100),
            SnapshotBreakpoint {
                condition: Some("Count > 1".to_string()),
                ..breakpoint("A.al", 12, 50100)
            },
        ]);
        let now = || SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let before = now().as_secs();
        let (result, calls) = Script {
            polls: vec![
                (
                    paused_at("A.al", 10, vec![integer("Count", "1")]),
                    Some((5, 50100)),
                ),
                (
                    paused_at("A.al", 12, vec![integer("Count", "2")]),
                    Some((5, 50100)),
                ),
                (
                    paused_at("A.al", 10, vec![integer("Count", "3")]),
                    Some((5, 50100)),
                ),
            ],
            ..script()
        }
        .run(&request)
        .await;
        let after = now().as_secs();
        let (snapshot, test_result) = result.expect("three stops at configured breakpoints");

        let count = |value: &str| serde_json::json!([{ "name": "Count", "value": value, "typeName": "Integer" }]);
        assert_eq!(
            snapshot.samples,
            vec![
                Sample {
                    breakpoint_id: 1010,
                    file: "A.al".to_string(),
                    line: 10,
                    condition: None,
                    iteration: 0,
                    variables: count("1"),
                },
                Sample {
                    breakpoint_id: 1012,
                    file: "A.al".to_string(),
                    line: 12,
                    condition: Some("Count > 1".to_string()),
                    iteration: 0,
                    variables: count("2"),
                },
                Sample {
                    breakpoint_id: 1010,
                    file: "A.al".to_string(),
                    line: 10,
                    condition: None,
                    iteration: 1,
                    variables: count("3"),
                },
            ]
        );
        assert_eq!(snapshot.codeunit_id, 50100);
        assert_eq!(snapshot.method_name, "Captures");
        assert_eq!(snapshot.bc_version, "26.0.0.0");
        assert_eq!(snapshot.source_hash, "abc");
        assert!(
            (before..=after).contains(&snapshot.captured_at),
            "captured_at {} is outside {before}..={after}",
            snapshot.captured_at
        );
        let nanos: u128 = snapshot
            .run_id
            .strip_prefix("50100-Captures-")
            .and_then(|nanos| nanos.parse().ok())
            .unwrap_or_else(|| panic!("unexpected run_id {}", snapshot.run_id));
        assert_eq!(nanos / 1_000_000_000, u128::from(snapshot.captured_at));
        assert_eq!(test_result.id, 50100);
        assert_eq!(test_result.methods[0].name, "Captures");
        assert_eq!(
            calls.iter().filter(|call| **call == Call::Continue).count(),
            3
        );
        assert_eq!(calls.last(), Some(&Call::Stop));
    }

    /// A running, a compiling and a stopped state that each point at the
    /// breakpoint on A.al line 10, then one pause there. Only the pause is
    /// sampled and continued, so the snapshot has one sample and one
    /// `Continue` was sent.
    #[tokio::test]
    async fn only_a_paused_session_is_sampled() {
        let request = request(vec![breakpoint("A.al", 10, 50100)]);
        let object = Some((5, 50100));
        let (result, calls) = Script {
            polls: vec![
                (state(SessionStatus::Running, Some(("A.al", 10))), object),
                (state(SessionStatus::Compiling, Some(("A.al", 10))), object),
                (state(SessionStatus::Stopped, Some(("A.al", 10))), object),
                (paused_at("A.al", 10, Vec::new()), object),
            ],
            ..script()
        }
        .run(&request)
        .await;
        let (snapshot, _) = result.expect("one pause at a configured breakpoint");
        assert_eq!(snapshot.samples.len(), 1);
        assert_eq!(
            calls.iter().filter(|call| **call == Call::Continue).count(),
            1
        );
    }

    /// Replies that give line 7 of A.al (object 50100) the ID 71 and line 7
    /// of B.al (object 50101) the ID 72.
    fn line_seven_in_two_files() -> (LiveSnapshotRequest, HashMap<String, Vec<BreakpointInfo>>) {
        let request = request(vec![
            breakpoint("A.al", 7, 50100),
            breakpoint("B.al", 7, 50101),
        ]);
        let replies = HashMap::from([
            ("A.al".to_string(), vec![verified("A.al", 7, 71)]),
            ("B.al".to_string(), vec![verified("B.al", 7, 72)]),
        ]);
        (request, replies)
    }

    /// Breakpoints on line 7 of A.al (object 50100, ID 71) and B.al (object
    /// 50101, ID 72), and a stop on line 7 that BC reports in object 50101
    /// with no file. The object picks B.al, so the sample has file B.al and
    /// ID 72.
    #[tokio::test]
    async fn a_stop_is_matched_by_the_object_bc_reports() {
        let (request, replies) = line_seven_in_two_files();
        let (result, _) = Script {
            replies,
            polls: vec![(paused_at("", 7, Vec::new()), Some((5, 50101)))],
            ..script()
        }
        .run(&request)
        .await;
        let (snapshot, _) = result.expect("the object picks one breakpoint");
        assert_eq!(snapshot.samples.len(), 1);
        assert_eq!(snapshot.samples[0].file, "B.al");
        assert_eq!(snapshot.samples[0].breakpoint_id, 72);
    }

    /// The same two breakpoints, and a stop on line 7 of B.al with no object.
    /// The file picks B.al, so the sample has file B.al and ID 72.
    #[tokio::test]
    async fn a_stop_without_an_object_is_matched_by_its_file() {
        let (request, replies) = line_seven_in_two_files();
        let (result, _) = Script {
            replies,
            polls: vec![(paused_at("B.al", 7, Vec::new()), None)],
            ..script()
        }
        .run(&request)
        .await;
        let (snapshot, _) = result.expect("the file picks one breakpoint");
        assert_eq!(snapshot.samples.len(), 1);
        assert_eq!(snapshot.samples[0].file, "B.al");
        assert_eq!(snapshot.samples[0].breakpoint_id, 72);
    }

    /// The same two breakpoints, and a stop on line 7 with neither an object
    /// nor a file. Both breakpoints match, so the capture fails with
    /// `AmbiguousStop` naming line 7, a count of 2 and both files, sends no
    /// `Continue` and stops the session.
    #[tokio::test]
    async fn a_stop_with_neither_object_nor_file_on_a_shared_line_is_ambiguous() {
        let (request, replies) = line_seven_in_two_files();
        let (result, calls) = Script {
            replies,
            polls: vec![(paused_at("", 7, Vec::new()), None)],
            ..script()
        }
        .run(&request)
        .await;
        let error = result.expect_err("two breakpoints match the stop");
        assert!(
            matches!(
                &error,
                SnapshotCaptureError::AmbiguousStop { line: 7, count: 2, files }
                    if files == &["A.al", "B.al"]
            ),
            "got {error:?}"
        );
        assert!(!calls.contains(&Call::Continue));
        assert_eq!(calls.last(), Some(&Call::Stop));
    }

    /// A breakpoint on A.al line 10 in object 50100, and three stops that
    /// each miss it: line 11 in object 50100, line 10 in object 50199, and
    /// line 10 of B.al with no object. Each fails with `UnexpectedStop`
    /// carrying the stop's line and object.
    #[tokio::test]
    async fn a_stop_that_matches_no_breakpoint_is_unexpected() {
        let request = request(vec![breakpoint("A.al", 10, 50100)]);
        let stops = [
            (paused_at("A.al", 11, Vec::new()), Some((5, 50100))),
            (paused_at("A.al", 10, Vec::new()), Some((5, 50199))),
            (paused_at("B.al", 10, Vec::new()), None),
        ];
        for (stop, object) in stops {
            let line = stop.location.as_ref().map(|location| location.line);
            let (result, calls) = Script {
                polls: vec![(stop, object)],
                ..script()
            }
            .run(&request)
            .await;
            let error = result.expect_err("the stop matches no breakpoint");
            assert!(
                matches!(
                    error,
                    SnapshotCaptureError::UnexpectedStop { line: stop_line, object: stop_object }
                        if Some(stop_line) == line && stop_object == object
                ),
                "line {line:?}, object {object:?}: got {error:?}"
            );
            assert_eq!(calls.last(), Some(&Call::Stop));
        }
    }

    /// A pause with no source location, in object 50100. The capture fails
    /// with `MissingStopLocation` naming that object.
    #[tokio::test]
    async fn a_pause_without_a_location_is_an_error() {
        let request = request(vec![breakpoint("A.al", 10, 50100)]);
        let (result, calls) = Script {
            polls: vec![(state(SessionStatus::Paused, None), Some((5, 50100)))],
            ..script()
        }
        .run(&request)
        .await;
        let error = result.expect_err("a pause needs a location");
        assert!(
            matches!(
                error,
                SnapshotCaptureError::MissingStopLocation {
                    object: Some((5, 50100))
                }
            ),
            "got {error:?}"
        );
        assert_eq!(calls.last(), Some(&Call::Stop));
    }

    /// A run that passes while the session never pauses. There are no
    /// samples, so the capture fails with `NoSamples` and stops the session.
    #[tokio::test]
    async fn a_run_that_hits_no_breakpoint_has_no_samples() {
        let request = request(vec![breakpoint("A.al", 10, 50100)]);
        let (result, calls) = script().run(&request).await;
        let error = result.expect_err("no stop was sampled");
        assert!(
            matches!(error, SnapshotCaptureError::NoSamples),
            "got {error:?}"
        );
        assert_eq!(calls.last(), Some(&Call::Stop));
    }

    /// One sample, and runs that report something other than exactly the
    /// method `Captures`: `Other`, `Captures` and `Other`, or no method. Each
    /// fails with `UnexpectedTestResult` listing the names that came back.
    #[tokio::test]
    async fn a_result_for_other_methods_is_rejected() {
        let request = request(vec![breakpoint("A.al", 10, 50100)]);
        let returned: [&[&str]; 3] = [&["Other"], &["Captures", "Other"], &[]];
        for methods in returned {
            let (result, _) = Script {
                polls: vec![(paused_at("A.al", 10, Vec::new()), Some((5, 50100)))],
                outcome: Some(Ok(result_for(methods))),
                ..script()
            }
            .run(&request)
            .await;
            let error = result.expect_err("the run must report exactly Captures");
            assert!(
                matches!(
                    &error,
                    SnapshotCaptureError::UnexpectedTestResult { expected, returned }
                        if expected == "Captures" && returned == methods
                ),
                "{methods:?}: got {error:?}"
            );
        }
    }

    /// One sample, and a run that reports `captures` in lower case. BC echoes
    /// a method name in its own casing, so the result is accepted.
    #[tokio::test]
    async fn the_method_name_in_the_result_is_matched_without_case() {
        let request = request(vec![breakpoint("A.al", 10, 50100)]);
        let (result, _) = Script {
            polls: vec![(paused_at("A.al", 10, Vec::new()), Some((5, 50100)))],
            outcome: Some(Ok(result_for(&["captures"]))),
            ..script()
        }
        .run(&request)
        .await;
        result.expect("captures is Captures in another casing");
    }

    /// Breakpoints on A.al lines 10 and 12, and replies that do not fit the
    /// request: one reply for the two lines is a count mismatch, and a reply
    /// for line 12 that is unverified or has ID 0 or -1 rejects line 12. Each
    /// fails before the runner connects, and the session is stopped.
    #[tokio::test]
    async fn breakpoint_replies_that_do_not_fit_the_request_are_errors() {
        let request = request(vec![
            breakpoint("A.al", 10, 50100),
            breakpoint("A.al", 12, 50100),
        ]);
        let unverified = BreakpointInfo {
            verified: false,
            ..verified("A.al", 12, 12)
        };
        let cases = [
            (vec![verified("A.al", 10, 10)], "count mismatch"),
            (
                vec![verified("A.al", 10, 10), unverified],
                "line 12 rejected",
            ),
            (
                vec![verified("A.al", 10, 10), verified("A.al", 12, 0)],
                "line 12 rejected",
            ),
            (
                vec![verified("A.al", 10, 10), verified("A.al", 12, -1)],
                "line 12 rejected",
            ),
        ];
        for (reply, expected) in cases {
            let (result, calls) = Script {
                replies: HashMap::from([("A.al".to_string(), reply.clone())]),
                ..script()
            }
            .run(&request)
            .await;
            let error = result.expect_err("the reply does not fit the request");
            let matched = match expected {
                "count mismatch" => matches!(
                    &error,
                    SnapshotCaptureError::BreakpointCountMismatch { file, expected: 2, returned: 1 }
                        if file == "A.al"
                ),
                _ => matches!(
                    &error,
                    SnapshotCaptureError::BreakpointRejected { file, line: 12 } if file == "A.al"
                ),
            };
            assert!(matched, "{reply:?}: expected {expected}, got {error:?}");
            assert!(!calls.contains(&Call::ConnectRunner), "{reply:?}");
            assert_eq!(calls.last(), Some(&Call::Stop), "{reply:?}");
        }
    }

    /// A breakpoint whose ID is 2^32, one more than the largest `u32`. The
    /// snapshot format stores IDs as `u32`, so the capture fails with
    /// `InvalidBreakpointId(4294967296)`.
    #[tokio::test]
    async fn a_breakpoint_id_above_u32_is_an_error() {
        let request = request(vec![breakpoint("A.al", 10, 50100)]);
        let (result, calls) = Script {
            replies: HashMap::from([(
                "A.al".to_string(),
                vec![verified("A.al", 10, 4_294_967_296)],
            )]),
            ..script()
        }
        .run(&request)
        .await;
        let error = result.expect_err("the ID does not fit in u32");
        assert!(
            matches!(
                error,
                SnapshotCaptureError::InvalidBreakpointId(4_294_967_296)
            ),
            "got {error:?}"
        );
        assert!(!calls.contains(&Call::ConnectRunner));
    }

    /// A breakpoint whose ID is 1 and one whose ID is 4294967295, the
    /// smallest and largest IDs a snapshot can store. Both are accepted, and
    /// a stop at each gives a sample with that ID.
    #[tokio::test]
    async fn the_smallest_and_largest_u32_breakpoint_ids_are_kept() {
        let request = request(vec![
            breakpoint("A.al", 10, 50100),
            breakpoint("A.al", 12, 50100),
        ]);
        let object = Some((5, 50100));
        let (result, _) = Script {
            replies: HashMap::from([(
                "A.al".to_string(),
                vec![verified("A.al", 10, 1), verified("A.al", 12, 4_294_967_295)],
            )]),
            polls: vec![
                (paused_at("A.al", 10, Vec::new()), object),
                (paused_at("A.al", 12, Vec::new()), object),
            ],
            ..script()
        }
        .run(&request)
        .await;
        let (snapshot, _) = result.expect("both IDs fit in u32");
        let ids = snapshot
            .samples
            .iter()
            .map(|sample| sample.breakpoint_id)
            .collect::<Vec<_>>();
        assert_eq!(ids, vec![1, u32::MAX]);
    }

    /// A 50 ms timeout, a session that never pauses and a run that never
    /// finishes. The capture fails with `Timeout(50)` and stops the session.
    #[tokio::test]
    async fn a_run_that_outlasts_the_timeout_is_an_error() {
        let request = LiveSnapshotRequest {
            timeout: Duration::from_millis(50),
            ..request(vec![breakpoint("A.al", 10, 50100)])
        };
        let (result, calls) = Script {
            outcome: None,
            ..script()
        }
        .run(&request)
        .await;
        let error = result.expect_err("the run never finishes");
        assert!(
            matches!(error, SnapshotCaptureError::Timeout(50)),
            "got {error:?}"
        );
        assert_eq!(calls.last(), Some(&Call::Stop));
    }

    /// A debug session that fails to start with "refused". The capture fails
    /// with that `Debug` error and does not connect the runner.
    #[tokio::test]
    async fn a_debug_session_that_fails_to_start_is_reported() {
        let connected = Cell::new(false);
        let result = capture(
            &request(vec![breakpoint("A.al", 10, 50100)]),
            async { Err::<FakeDebugger, _>(DapError::ConnectionFailed("refused".to_string())) },
            || {
                connected.set(true);
                Err::<FakeRunner, _>(TestRunnerError::MissingCredentials)
            },
        )
        .await;
        let error = result.expect_err("the session did not start");
        assert!(
            matches!(
                &error,
                SnapshotCaptureError::Debug(DapError::ConnectionFailed(message))
                    if message == "refused"
            ),
            "got {error:?}"
        );
        assert!(!connected.get(), "the runner must not connect");
    }

    /// A runner that fails to connect with `MissingCredentials` once the
    /// breakpoints are set. The capture fails with that `Test` error, runs no
    /// test and stops the session.
    #[tokio::test]
    async fn a_runner_that_fails_to_connect_still_stops_the_session() {
        let request = request(vec![breakpoint("A.al", 10, 50100)]);
        let (result, calls) = Script {
            connect_error: Some(TestRunnerError::MissingCredentials),
            ..script()
        }
        .run(&request)
        .await;
        let error = result.expect_err("the runner did not connect");
        assert!(
            matches!(
                error,
                SnapshotCaptureError::Test(TestRunnerError::MissingCredentials)
            ),
            "got {error:?}"
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| matches!(call, Call::RunCodeunit { .. }))
                .count(),
            0
        );
        assert_eq!(calls.last(), Some(&Call::Stop));
    }

    /// One stop, then a run that fails with HTTP 500 "boom". The capture fails
    /// with that `Test` error and stops the session.
    #[tokio::test]
    async fn a_run_that_fails_ends_the_capture() {
        let request = request(vec![breakpoint("A.al", 10, 50100)]);
        let (result, calls) = Script {
            polls: vec![(paused_at("A.al", 10, Vec::new()), Some((5, 50100)))],
            outcome: Some(Err(TestRunnerError::ServerError {
                status: 500,
                message: "boom".to_string(),
            })),
            ..script()
        }
        .run(&request)
        .await;
        let error = result.expect_err("the run failed");
        assert!(
            matches!(
                &error,
                SnapshotCaptureError::Test(TestRunnerError::ServerError { status: 500, message })
                    if message == "boom"
            ),
            "got {error:?}"
        );
        assert_eq!(calls.last(), Some(&Call::Stop));
    }

    /// One stop at the breakpoint, and a session that fails to stop with
    /// "hub closed". The snapshot is complete, so it is returned with its
    /// one sample.
    #[tokio::test]
    async fn a_session_that_fails_to_stop_keeps_a_complete_snapshot() {
        let request = request(vec![breakpoint("A.al", 10, 50100)]);
        let (result, _) = Script {
            polls: vec![(paused_at("A.al", 10, Vec::new()), Some((5, 50100)))],
            stop_error: Some(DapError::ConnectionFailed("hub closed".to_string())),
            ..script()
        }
        .run(&request)
        .await;
        let (snapshot, _) = result.expect("the snapshot is complete");
        assert_eq!(snapshot.samples.len(), 1);
    }

    /// A run with no stop, and a session that fails to stop with "hub
    /// closed". Both failures are reported as `CaptureAndShutdown`, with the
    /// `NoSamples` message and the `ConnectionFailed` message as their
    /// display text.
    #[tokio::test]
    async fn a_failed_capture_and_a_failed_stop_are_both_reported() {
        let request = request(vec![breakpoint("A.al", 10, 50100)]);
        let (result, _) = Script {
            stop_error: Some(DapError::ConnectionFailed("hub closed".to_string())),
            ..script()
        }
        .run(&request)
        .await;
        let error = result.expect_err("both steps failed");
        assert!(
            matches!(
                &error,
                SnapshotCaptureError::CaptureAndShutdown { capture, shutdown }
                    if capture == "live test completed without hitting any configured breakpoint"
                        && shutdown == "Connection failed: hub closed"
            ),
            "got {error:?}"
        );
    }
}
