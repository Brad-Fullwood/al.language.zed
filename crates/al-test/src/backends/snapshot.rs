//! Live Business Central test snapshot capture.
//!
//! This backend deliberately composes the existing BC test runner and native
//! debug hub instead of pretending a local interpreter sample is equivalent.
//! The caller supplies resolved workspace breakpoint metadata and is
//! responsible for persisting the returned `al_snapshot::Snapshot`.

use std::collections::HashMap;
use std::time::Duration;

use al_bc::launch::BcServerConfig;
use al_dap::dap::bc_debug::BcDebugConfig;
use al_dap::dap::types::SessionStatus;
use al_dap::native_debug::NativeDebugSession;
use al_snapshot::{Sample, Snapshot};
use thiserror::Error;

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

pub async fn capture_live_snapshot(
    request: LiveSnapshotRequest,
) -> Result<(Snapshot, TestCodeunitResult), SnapshotCaptureError> {
    if request.breakpoints.is_empty() {
        return Err(SnapshotCaptureError::MissingBreakpoints);
    }

    let mut debug = NativeDebugSession::start(request.debug.clone(), &request.access_token).await?;
    let capture_result = capture_with_session(&request, &mut debug).await;
    let shutdown_result = debug.stop().await;
    match (capture_result, shutdown_result) {
        (Ok(snapshot), Ok(())) => Ok(snapshot),
        (Ok(_), Err(shutdown)) => Err(SnapshotCaptureError::Debug(shutdown)),
        (Err(capture), Ok(())) => Err(capture),
        (Err(capture), Err(shutdown)) => Err(SnapshotCaptureError::CaptureAndShutdown {
            capture: capture.to_string(),
            shutdown: shutdown.to_string(),
        }),
    }
}

async fn capture_with_session(
    request: &LiveSnapshotRequest,
    debug: &mut NativeDebugSession,
) -> Result<(Snapshot, TestCodeunitResult), SnapshotCaptureError> {
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

    let client = TestRunnerClient::with_access_token(&request.server, Some(&request.access_token))?;
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
                let state = debug.state().await?;
                if state.status != SessionStatus::Paused {
                    continue;
                }
                let current_object = debug.current_object();
                let location = state
                    .location
                    .ok_or(SnapshotCaptureError::MissingStopLocation {
                        object: current_object,
                    })?;
                let candidates = request
                    .breakpoints
                    .iter()
                    .filter(|breakpoint| {
                        breakpoint.line == location.line
                            && current_object.is_none_or(|(object_type, object_id)| {
                                breakpoint.object_type == object_type
                                    && breakpoint.object_id == object_id
                            })
                    })
                    .collect::<Vec<_>>();
                let configured = match candidates.as_slice() {
                    [breakpoint] => *breakpoint,
                    _ => {
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
}
