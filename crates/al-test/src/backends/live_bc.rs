//! Live BC backend — runs tests against a real Business Central server.
//!
//! `LiveBcMode` implements `TestSession`. It groups `TestId`s by codeunit,
//! optionally dispatches codeunit runs in parallel via `tokio::task::JoinSet`,
//! applies per-codeunit timeouts, and streams `TestEvent`s through the
//! caller-supplied `mpsc::Sender`.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::error::TestRunnerError;
use crate::result::{TestCodeunitResult, TestMethodResult, TestStatus};
use crate::session::{RunOptions, TestEvent, TestId, TestSession};
use crate::test_runner::TestRunnerClient;
use al_bc::launch::BcServerConfig;

pub struct LiveBcMode {
    config: BcServerConfig,
}

impl LiveBcMode {
    pub fn new(config: BcServerConfig) -> Self {
        Self { config }
    }
}

/// Match `name` against a simple-glob `pattern`. Supports `*` (zero-or-more
/// of any char) and is case-insensitive — matches AL's identifier rules.
/// No-asterisk patterns require an exact case-insensitive match.
fn method_matches(name: &str, pattern: &str) -> bool {
    let n = name.to_ascii_lowercase();
    let p = pattern.to_ascii_lowercase();
    if !p.contains('*') {
        return n == p;
    }
    // Iteratively consume `*`-separated literal chunks left-to-right.
    let mut cursor = 0usize;
    let mut first_chunk = true;
    let leading_star = p.starts_with('*');
    let trailing_star = p.ends_with('*');
    let chunks: Vec<&str> = p.split('*').filter(|s| !s.is_empty()).collect();
    for chunk in &chunks {
        if first_chunk && !leading_star {
            if !n[cursor..].starts_with(chunk) {
                return false;
            }
            cursor += chunk.len();
        } else {
            match n[cursor..].find(chunk) {
                Some(idx) => cursor += idx + chunk.len(),
                None => return false,
            }
        }
        first_chunk = false;
    }
    if !trailing_star && cursor != n.len() {
        return false;
    }
    true
}

async fn send_event(tx: &mpsc::Sender<TestEvent>, event: TestEvent) -> Result<(), TestRunnerError> {
    tx.send(event).await.map_err(|_| {
        tracing::warn!("test event channel closed; receiver dropped — aborting run");
        TestRunnerError::ChannelClosed
    })
}

async fn run_one_codeunit(
    config: &BcServerConfig,
    codeunit_id: i32,
    codeunit_name: &str,
    methods: &[Option<String>],
    timeout_dur: Duration,
) -> Vec<TestEvent> {
    let client = TestRunnerClient::new(config);
    let mut events = Vec::new();

    let has_specific_methods = methods.iter().any(|m| m.is_some());

    if has_specific_methods {
        for method_opt in methods {
            let method_str = method_opt.as_deref();
            let method_display = method_str.unwrap_or(codeunit_name).to_string();

            let started_id = TestId {
                codeunit_id,
                codeunit_name: codeunit_name.to_string(),
                method_name: method_str.map(str::to_string),
            };
            events.push(TestEvent::CaseStarted { id: started_id });

            match tokio::time::timeout(
                timeout_dur,
                client.run_codeunit(codeunit_id, codeunit_name, method_str),
            )
            .await
            {
                Ok(Ok(result)) => {
                    for method_result in &result.methods {
                        let id = TestId {
                            codeunit_id,
                            codeunit_name: codeunit_name.to_string(),
                            method_name: Some(method_result.name.clone()),
                        };
                        events.push(TestEvent::CaseResult {
                            id,
                            result: method_result.clone(),
                        });
                    }
                    events.push(TestEvent::SuiteComplete {
                        codeunit_id,
                        summary: result,
                    });
                }
                Ok(Err(e)) => {
                    events.push(TestEvent::Error {
                        message: format!("{e}"),
                    });
                    // Emit a SuiteComplete with zero counts so tally stays consistent.
                    let empty = TestCodeunitResult {
                        name: codeunit_name.to_string(),
                        id: codeunit_id,
                        methods: vec![],
                        total: 0,
                        passed: 0,
                        failed: 0,
                        skipped: 0,
                    };
                    events.push(TestEvent::SuiteComplete {
                        codeunit_id,
                        summary: empty,
                    });
                }
                Err(_elapsed) => {
                    let skip_result = TestMethodResult {
                        name: method_display,
                        status: TestStatus::Skip,
                        error: Some(format!("timeout after {} ms", timeout_dur.as_millis())),
                        duration_ms: None,
                    };
                    let id = TestId {
                        codeunit_id,
                        codeunit_name: codeunit_name.to_string(),
                        method_name: method_str.map(str::to_string),
                    };
                    events.push(TestEvent::CaseResult {
                        id,
                        result: skip_result.clone(),
                    });
                    let summary = TestCodeunitResult {
                        name: codeunit_name.to_string(),
                        id: codeunit_id,
                        methods: vec![skip_result],
                        total: 1,
                        passed: 0,
                        failed: 0,
                        skipped: 1,
                    };
                    events.push(TestEvent::SuiteComplete {
                        codeunit_id,
                        summary,
                    });
                }
            }
        }
    } else {
        let started_id = TestId {
            codeunit_id,
            codeunit_name: codeunit_name.to_string(),
            method_name: None,
        };
        events.push(TestEvent::CaseStarted { id: started_id });

        match tokio::time::timeout(
            timeout_dur,
            client.run_codeunit(codeunit_id, codeunit_name, None),
        )
        .await
        {
            Ok(Ok(result)) => {
                for method_result in &result.methods {
                    let id = TestId {
                        codeunit_id,
                        codeunit_name: codeunit_name.to_string(),
                        method_name: Some(method_result.name.clone()),
                    };
                    events.push(TestEvent::CaseResult {
                        id,
                        result: method_result.clone(),
                    });
                }
                events.push(TestEvent::SuiteComplete {
                    codeunit_id,
                    summary: result,
                });
            }
            Ok(Err(e)) => {
                events.push(TestEvent::Error {
                    message: format!("{e}"),
                });
                let empty = TestCodeunitResult {
                    name: codeunit_name.to_string(),
                    id: codeunit_id,
                    methods: vec![],
                    total: 0,
                    passed: 0,
                    failed: 0,
                    skipped: 0,
                };
                events.push(TestEvent::SuiteComplete {
                    codeunit_id,
                    summary: empty,
                });
            }
            Err(_elapsed) => {
                let skip_result = TestMethodResult {
                    name: codeunit_name.to_string(),
                    status: TestStatus::Skip,
                    error: Some(format!("timeout after {} ms", timeout_dur.as_millis())),
                    duration_ms: None,
                };
                let id = TestId {
                    codeunit_id,
                    codeunit_name: codeunit_name.to_string(),
                    method_name: None,
                };
                events.push(TestEvent::CaseResult {
                    id,
                    result: skip_result.clone(),
                });
                let summary = TestCodeunitResult {
                    name: codeunit_name.to_string(),
                    id: codeunit_id,
                    methods: vec![skip_result],
                    total: 1,
                    passed: 0,
                    failed: 0,
                    skipped: 1,
                };
                events.push(TestEvent::SuiteComplete {
                    codeunit_id,
                    summary,
                });
            }
        }
    }

    events
}

impl TestSession for LiveBcMode {
    async fn run(
        &self,
        tests: Vec<TestId>,
        opts: RunOptions,
        tx: mpsc::Sender<TestEvent>,
    ) -> Result<(), TestRunnerError> {
        let timeout_dur = Duration::from_millis(opts.timeout_ms.unwrap_or(30_000));

        // Apply method-name filter (simple `*` wildcard, case-insensitive).
        // A None method (whole-codeunit run) is always kept — the filter
        // can't disambiguate at this layer without first discovering
        // method names.
        let tests = match opts.filter.as_deref() {
            None => tests,
            Some(pattern) => tests
                .into_iter()
                .filter(|t| match &t.method_name {
                    None => true,
                    Some(m) => method_matches(m, pattern),
                })
                .collect(),
        };

        // Deduplicate identical (codeunit_id, method_name) targets before
        // grouping. A malformed RPC call can repeat the same TestId; without
        // this guard the BC API would be invoked once per duplicate and every
        // duplicate's SuiteComplete summary would be tallied into the final
        // SessionComplete, inflating total/passed/failed/skipped.
        let mut seen: HashSet<(i32, Option<String>)> = HashSet::new();

        let mut groups: HashMap<i32, (String, Vec<Option<String>>)> = HashMap::new();
        for test in tests {
            if !seen.insert((test.codeunit_id, test.method_name.clone())) {
                continue;
            }
            let entry = groups
                .entry(test.codeunit_id)
                .or_insert_with(|| (test.codeunit_name.clone(), Vec::new()));
            entry.1.push(test.method_name);
        }

        let codeunits: Vec<(i32, String, Vec<Option<String>>)> = groups
            .into_iter()
            .map(|(id, (name, methods))| (id, name, methods))
            .collect();

        let mut total_total: usize = 0;
        let mut total_passed: usize = 0;
        let mut total_failed: usize = 0;
        let mut total_skipped: usize = 0;

        if opts.parallel && codeunits.len() > 1 {
            let mut join_set: JoinSet<Vec<TestEvent>> = JoinSet::new();
            for (codeunit_id, codeunit_name, methods) in codeunits {
                let config = self.config.clone();
                join_set.spawn(async move {
                    run_one_codeunit(&config, codeunit_id, &codeunit_name, &methods, timeout_dur)
                        .await
                });
            }

            while let Some(result) = join_set.join_next().await {
                let events = result.unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "codeunit task panicked or was aborted; dropping its events");
                    Vec::new()
                });
                for event in events {
                    if let TestEvent::SuiteComplete { ref summary, .. } = event {
                        total_total += summary.total;
                        total_passed += summary.passed;
                        total_failed += summary.failed;
                        total_skipped += summary.skipped;
                    }
                    send_event(&tx, event).await?;
                }
            }
        } else {
            for (codeunit_id, codeunit_name, methods) in codeunits {
                let events = run_one_codeunit(
                    &self.config,
                    codeunit_id,
                    &codeunit_name,
                    &methods,
                    timeout_dur,
                )
                .await;
                for event in events {
                    if let TestEvent::SuiteComplete { ref summary, .. } = event {
                        total_total += summary.total;
                        total_passed += summary.passed;
                        total_failed += summary.failed;
                        total_skipped += summary.skipped;
                    }
                    send_event(&tx, event).await?;
                }
            }
        }

        send_event(
            &tx,
            TestEvent::SessionComplete {
                total: total_total,
                passed: total_passed,
                failed: total_failed,
                skipped: total_skipped,
            },
        )
        .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::backends::live_bc::LiveBcMode;
    use crate::error::TestRunnerError;
    use crate::result::TestStatus;
    use crate::session::{RunOptions, TestEvent, TestId, TestSession};
    use al_bc::launch::{AuthMethod, BcServerConfig, EnvironmentType};

    /// Build a `BcServerConfig` pointing at the given wiremock base URL.
    ///
    /// Uses Windows auth so no env-var credentials are required (the mock
    /// server accepts any request without checking auth headers).
    fn config_for(base_url: &str) -> BcServerConfig {
        BcServerConfig {
            name: "test".into(),
            environment_type: EnvironmentType::OnPrem,
            server: Some(base_url.to_string()),
            server_instance: Some("BC".into()),
            port: None,
            environment_name: None,
            tenant: None,
            authentication: AuthMethod::Windows,
            accept_invalid_certs: false,
        }
    }

    fn test_id(codeunit_id: i32, name: &str) -> TestId {
        TestId {
            codeunit_id,
            codeunit_name: name.into(),
            method_name: None,
        }
    }

    use super::method_matches;

    #[test]
    fn method_matches_exact_case_insensitive() {
        assert!(method_matches("TestAlpha", "TestAlpha"));
        assert!(method_matches("TestAlpha", "testalpha"));
        assert!(method_matches("TESTALPHA", "TestAlpha"));
        assert!(!method_matches("TestAlpha", "Alpha"));
        assert!(!method_matches("TestAlphaExtra", "TestAlpha"));
    }

    #[test]
    fn method_matches_leading_wildcard() {
        assert!(method_matches("TestAlpha", "*Alpha"));
        assert!(method_matches("Alpha", "*Alpha"));
        assert!(!method_matches("AlphaTest", "*Alpha"));
    }

    #[test]
    fn method_matches_trailing_wildcard() {
        assert!(method_matches("TestAlpha", "Test*"));
        assert!(method_matches("Test", "Test*"));
        assert!(!method_matches("UnitTest", "Test*"));
    }

    #[test]
    fn method_matches_middle_wildcard() {
        assert!(method_matches("TestAlphaCase", "Test*Case"));
        assert!(method_matches("TestCase", "Test*Case"));
        // Negative: pattern with middle wildcard requires both anchors.
        assert!(!method_matches("Test", "Test*Case"));
        assert!(!method_matches("Case", "Test*Case"));
    }

    #[test]
    fn method_matches_double_star() {
        assert!(method_matches("AlphaBeta", "*Beta*"));
        assert!(method_matches("BetaAlpha", "*Beta*"));
        assert!(method_matches("XBetaY", "*Beta*"));
        assert!(!method_matches("Gamma", "*Beta*"));
    }

    #[test]
    fn method_matches_just_star_matches_anything() {
        assert!(method_matches("anything", "*"));
        assert!(method_matches("", "*"));
    }

    #[tokio::test]
    async fn test_live_bc_mode_constructs_from_config() {
        let server = MockServer::start().await;
        let cfg = config_for(&server.uri());
        let _mode = LiveBcMode::new(cfg);
    }

    #[tokio::test]
    async fn test_live_bc_mode_empty_tests_emits_only_session_complete() {
        let server = MockServer::start().await;
        let cfg = config_for(&server.uri());
        let mode = LiveBcMode::new(cfg);

        let (tx, mut rx) = mpsc::channel::<TestEvent>(16);
        mode.run(vec![], RunOptions::default(), tx).await.unwrap();

        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }

        assert_eq!(
            events.len(),
            1,
            "expected exactly one event, got {events:?}"
        );
        match &events[0] {
            TestEvent::SessionComplete {
                total,
                passed,
                failed,
                skipped,
            } => {
                assert_eq!(*total, 0);
                assert_eq!(*passed, 0);
                assert_eq!(*failed, 0);
                assert_eq!(*skipped, 0);
            }
            other => panic!("expected SessionComplete, got {other:?}"),
        }

        assert_eq!(server.received_requests().await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_live_bc_mode_single_codeunit_pass_path() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/BC/dev/tests/50100/run"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [
                    { "name": "TestA", "result": "pass", "duration": 0.05 }
                ]
            })))
            .mount(&server)
            .await;

        let cfg = config_for(&server.uri());
        let mode = LiveBcMode::new(cfg);

        let (tx, mut rx) = mpsc::channel::<TestEvent>(32);
        mode.run(vec![test_id(50100, "MyTests")], RunOptions::default(), tx)
            .await
            .unwrap();

        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }

        assert!(
            events.len() >= 4,
            "expected ≥4 events, got {}: {events:?}",
            events.len()
        );

        match &events[0] {
            TestEvent::CaseStarted { id } => {
                assert_eq!(id.codeunit_id, 50100, "wrong codeunit_id in CaseStarted");
            }
            other => panic!("expected CaseStarted, got {other:?}"),
        }

        let case_result = events
            .iter()
            .find(|e| matches!(e, TestEvent::CaseResult { .. }));
        match case_result {
            Some(TestEvent::CaseResult { result, .. }) => {
                assert_eq!(result.status, TestStatus::Pass, "expected Pass status");
            }
            _ => panic!("no CaseResult event found in {events:?}"),
        }

        let suite = events.iter().find(|e| {
            matches!(
                e,
                TestEvent::SuiteComplete {
                    codeunit_id: 50100,
                    ..
                }
            )
        });
        match suite {
            Some(TestEvent::SuiteComplete { summary, .. }) => {
                assert_eq!(summary.passed, 1, "expected 1 passed in SuiteComplete");
            }
            _ => panic!("no SuiteComplete for codeunit 50100 in {events:?}"),
        }

        let last = events.last().unwrap();
        match last {
            TestEvent::SessionComplete {
                total,
                passed,
                failed,
                skipped,
            } => {
                assert_eq!(*total, 1);
                assert_eq!(*passed, 1);
                assert_eq!(*failed, 0);
                assert_eq!(*skipped, 0);
            }
            other => panic!("expected SessionComplete last, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_live_bc_mode_two_codeunits_parallel() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/BC/dev/tests/50100/run"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [{ "name": "TestA", "result": "pass", "duration": 0.01 }]
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/BC/dev/tests/50200/run"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [{ "name": "TestB", "result": "pass", "duration": 0.01 }]
            })))
            .mount(&server)
            .await;

        let cfg = config_for(&server.uri());
        let mode = LiveBcMode::new(cfg);

        let opts = RunOptions {
            parallel: true,
            ..Default::default()
        };

        let (tx, mut rx) = mpsc::channel::<TestEvent>(64);
        mode.run(
            vec![test_id(50100, "Suite1"), test_id(50200, "Suite2")],
            opts,
            tx,
        )
        .await
        .unwrap();

        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }

        let suite_100 = events.iter().any(|e| {
            matches!(
                e,
                TestEvent::SuiteComplete {
                    codeunit_id: 50100,
                    ..
                }
            )
        });
        let suite_200 = events.iter().any(|e| {
            matches!(
                e,
                TestEvent::SuiteComplete {
                    codeunit_id: 50200,
                    ..
                }
            )
        });
        assert!(suite_100, "missing SuiteComplete for 50100 in {events:?}");
        assert!(suite_200, "missing SuiteComplete for 50200 in {events:?}");

        match events.last() {
            Some(TestEvent::SessionComplete { total, .. }) => {
                assert_eq!(*total, 2, "expected total=2 in SessionComplete");
            }
            other => panic!("expected SessionComplete last, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_live_bc_mode_per_test_timeout() {
        use std::time::Duration;

        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/BC/dev/tests/50100/run"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(500))
                    .set_body_json(serde_json::json!({ "value": [] })),
            )
            .mount(&server)
            .await;

        let cfg = config_for(&server.uri());
        let mode = LiveBcMode::new(cfg);

        let opts = RunOptions {
            timeout_ms: Some(50), // 50ms → fires well before 500ms mock delay
            ..Default::default()
        };

        let (tx, mut rx) = mpsc::channel::<TestEvent>(32);
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            mode.run(vec![test_id(50100, "SlowSuite")], opts, tx),
        )
        .await
        .expect("run() must not hang — timed out after 5s");

        // run() itself should succeed (the timeout is internal, not fatal).
        result.unwrap_or_else(|e| {
            // ChannelClosed is acceptable if receiver was dropped before SessionComplete.
            match e {
                TestRunnerError::ChannelClosed => {}
                other => panic!("unexpected error from run(): {other}"),
            }
        });

        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }

        let skip_result = events.iter().find(|e| {
            if let TestEvent::CaseResult { result, .. } = e {
                result.status == TestStatus::Skip
                    && result
                        .error
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains("timeout")
            } else {
                false
            }
        });
        assert!(
            skip_result.is_some(),
            "expected a Skip CaseResult with timeout message, got {events:?}"
        );

        let session_complete = events
            .iter()
            .find(|e| matches!(e, TestEvent::SessionComplete { .. }));
        match session_complete {
            Some(TestEvent::SessionComplete { skipped, .. }) => {
                assert!(*skipped >= 1, "expected skipped >= 1, got {skipped}");
            }
            None => panic!("no SessionComplete in {events:?}"),
            _ => unreachable!(),
        }
    }

    /// Passing the same TestId twice runs the codeunit once and reports one test.
    #[tokio::test]
    async fn test_live_bc_mode_duplicate_test_ids_deduplicated() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/BC/dev/tests/50100/run"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [{ "name": "TestA", "result": "pass", "duration": 0.01 }]
            })))
            .mount(&server)
            .await;

        let cfg = config_for(&server.uri());
        let mode = LiveBcMode::new(cfg);

        let (tx, mut rx) = mpsc::channel::<TestEvent>(32);
        // Same whole-codeunit TestId twice.
        mode.run(
            vec![test_id(50100, "MyTests"), test_id(50100, "MyTests")],
            RunOptions::default(),
            tx,
        )
        .await
        .unwrap();

        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }

        assert_eq!(
            server.received_requests().await.unwrap().len(),
            1,
            "duplicate TestId must not cause a second BC call"
        );

        let suite_count = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    TestEvent::SuiteComplete {
                        codeunit_id: 50100,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(suite_count, 1, "expected one SuiteComplete, got {events:?}");

        match events.last() {
            Some(TestEvent::SessionComplete {
                total,
                passed,
                failed,
                skipped,
            }) => {
                assert_eq!(*total, 1, "total must not be inflated by the duplicate");
                assert_eq!(*passed, 1);
                assert_eq!(*failed, 0);
                assert_eq!(*skipped, 0);
            }
            other => panic!("expected SessionComplete last, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_live_bc_mode_server_500_emits_error_event_and_continues() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/BC/dev/tests/50100/run"))
            .respond_with(ResponseTemplate::new(500).set_body_string("internal server error"))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/BC/dev/tests/50200/run"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [{ "name": "TestOK", "result": "pass", "duration": 0.01 }]
            })))
            .mount(&server)
            .await;

        let cfg = config_for(&server.uri());
        let mode = LiveBcMode::new(cfg);

        let (tx, mut rx) = mpsc::channel::<TestEvent>(64);
        mode.run(
            vec![test_id(50100, "BrokenSuite"), test_id(50200, "GoodSuite")],
            RunOptions::default(),
            tx,
        )
        .await
        .unwrap();

        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }

        let error_event = events.iter().find(|e| {
            if let TestEvent::Error { message } = e {
                message.contains("500")
            } else {
                false
            }
        });
        assert!(
            error_event.is_some(),
            "expected Error event containing '500', got {events:?}"
        );

        let suite_200 = events.iter().any(|e| {
            matches!(
                e,
                TestEvent::SuiteComplete {
                    codeunit_id: 50200,
                    ..
                }
            )
        });
        assert!(
            suite_200,
            "expected SuiteComplete for codeunit 50200 (run must continue after 500), got {events:?}"
        );

        assert!(
            events
                .iter()
                .any(|e| matches!(e, TestEvent::SessionComplete { .. })),
            "expected SessionComplete, got {events:?}"
        );
    }
}
